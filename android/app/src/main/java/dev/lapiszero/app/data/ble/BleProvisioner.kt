package dev.lapiszero.app.data.ble

import android.Manifest
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelUuid
import android.util.Log
import androidx.core.content.ContextCompat
import java.io.IOException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.receiveAsFlow
import no.nordicsemi.android.ble.BleManager
import no.nordicsemi.android.ble.ktx.suspend
import no.nordicsemi.android.ble.observer.ConnectionObserver

/** A CardputerZero advertising the provisioning service. */
class FoundDevice(val device: BluetoothDevice, val name: String, val rssi: Int) {
    val address: String get() = device.address
}

/** Runtime permissions the BLE path needs on this Android version. */
object BlePermissions {
    fun required(): Array<String> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            arrayOf(Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_CONNECT)
        } else {
            // Android 10 and 11 deliver BLE scan results only with fine location.
            arrayOf(Manifest.permission.ACCESS_FINE_LOCATION)
        }

    fun granted(context: Context): Boolean = required().all {
        ContextCompat.checkSelfPermission(context, it) == PackageManager.PERMISSION_GRANTED
    }
}

/**
 * Scanner and GATT client for `lapis-ble-v1`: finds devices advertising the service UUID,
 * connects, requests ATT MTU 517 and exposes the link as a [BleTransport]. All protocol logic
 * lives in [ProvisioningClient]; this class only moves bytes.
 */
class BleProvisioner(context: Context) {
    private val appContext = context.applicationContext

    private val adapter: BluetoothAdapter?
        get() = appContext.getSystemService(BluetoothManager::class.java)?.adapter

    val isSupported: Boolean
        get() = adapter != null &&
            appContext.packageManager.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)

    val isEnabled: Boolean get() = adapter?.isEnabled == true

    /**
     * Emits the growing, signal-sorted list of devices advertising the provisioning service.
     * Fails with [IOException] when Bluetooth is off, unsupported or not permitted.
     */
    fun scan(): Flow<List<FoundDevice>> = callbackFlow {
        if (!BlePermissions.granted(appContext)) throw IOException("Bluetooth permission was not granted")
        val scanner = adapter?.takeIf { it.isEnabled }?.bluetoothLeScanner
            ?: throw IOException("Bluetooth is turned off")
        val found = LinkedHashMap<String, FoundDevice>()
        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                // The advertised local name needs no extra permission, unlike BluetoothDevice.name.
                val name = result.scanRecord?.deviceName?.takeIf { it.isNotBlank() } ?: "Lapis device"
                found[result.device.address] = FoundDevice(result.device, name, result.rssi)
                trySend(found.values.sortedByDescending { it.rssi })
            }

            override fun onScanFailed(errorCode: Int) {
                close(IOException("Bluetooth scan failed (code $errorCode)"))
            }
        }
        val filters = listOf(ScanFilter.Builder().setServiceUuid(ParcelUuid(BleUuids.SERVICE)).build())
        val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build()
        try {
            scanner.startScan(filters, settings, callback)
        } catch (e: SecurityException) {
            throw IOException("Bluetooth permission was not granted", e)
        }
        trySend(emptyList())
        awaitClose {
            try {
                scanner.stopScan(callback)
            } catch (e: SecurityException) {
                // Permission revoked mid-scan: there is nothing left to stop.
            } catch (e: IllegalStateException) {
                // Bluetooth was switched off while scanning.
            }
        }
    }

    /** Connects, discovers the service, negotiates the MTU and enables EVT_RX notifications. */
    suspend fun connect(device: BluetoothDevice): BleLink {
        if (!BlePermissions.granted(appContext)) throw IOException("Bluetooth permission was not granted")
        val manager = LapisBleManager(appContext)
        try {
            manager.open(device)
        } catch (e: kotlinx.coroutines.CancellationException) {
            manager.shutdown()
            throw e
        } catch (e: Exception) {
            manager.shutdown()
            throw IOException("Could not connect to the device over Bluetooth", e)
        }
        return BleLink(manager)
    }
}

/** An open GATT connection. Always [close] it. */
class BleLink internal constructor(private val manager: LapisBleManager) : BleTransport by manager {
    suspend fun close() = manager.shutdown()
}

internal class LapisBleManager(context: Context) : BleManager(context), BleTransport {
    private var handshake: BluetoothGattCharacteristic? = null
    private var cmdTx: BluetoothGattCharacteristic? = null
    private var evtRx: BluetoothGattCharacteristic? = null
    private val incoming = Channel<ByteArray>(Channel.UNLIMITED)

    override val attMtu: Int get() = mtu

    /** Completes when the link drops, which is how [ProvisioningClient] learns about it. */
    override val notifications: Flow<ByteArray> = incoming.receiveAsFlow()

    init {
        connectionObserver = object : ConnectionObserver {
            override fun onDeviceConnecting(device: BluetoothDevice) = Unit
            override fun onDeviceConnected(device: BluetoothDevice) = Unit
            override fun onDeviceReady(device: BluetoothDevice) = Unit
            override fun onDeviceDisconnecting(device: BluetoothDevice) = Unit

            override fun onDeviceFailedToConnect(device: BluetoothDevice, reason: Int) {
                incoming.close()
            }

            override fun onDeviceDisconnected(device: BluetoothDevice, reason: Int) {
                incoming.close()
            }
        }
    }

    // GATT traffic carries (encrypted) credentials and the library logs payloads in hex:
    // keep all of it out of logcat (nothing in the library logs at ASSERT, and log() is a no-op).
    override fun getMinLogPriority(): Int = Log.ASSERT

    override fun log(priority: Int, message: String) = Unit

    override fun isRequiredServiceSupported(gatt: BluetoothGatt): Boolean {
        val service = gatt.getService(BleUuids.SERVICE) ?: return false
        handshake = service.getCharacteristic(BleUuids.HANDSHAKE)
        cmdTx = service.getCharacteristic(BleUuids.CMD_TX)
        evtRx = service.getCharacteristic(BleUuids.EVT_RX)
        return handshake != null && cmdTx != null && evtRx != null
    }

    override fun initialize() {
        requestMtu(BleUuids.REQUESTED_MTU).enqueue()
        setNotificationCallback(evtRx).with { _, data -> data.value?.let { incoming.trySend(it) } }
        enableNotifications(evtRx).enqueue()
    }

    override fun onServicesInvalidated() {
        handshake = null
        cmdTx = null
        evtRx = null
    }

    suspend fun open(device: BluetoothDevice) {
        connect(device).retry(3, 300).timeout(20_000).useAutoConnect(false).suspend()
    }

    suspend fun shutdown() {
        try {
            if (isConnected) disconnect().suspend()
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            // Already gone.
        } finally {
            incoming.close()
            close()
        }
    }

    override suspend fun writeHandshake(clientHello: ByteArray) {
        writeCharacteristic(handshake, clientHello, BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT).suspend()
    }

    override suspend fun readHandshake(): ByteArray =
        readCharacteristic(handshake).suspend().value ?: ByteArray(0)

    override suspend fun writeCommand(frame: ByteArray) {
        // One GATT write per protocol frame (the frames are already sized to the MTU), with
        // response, so frames arrive in order and a dead link surfaces as an error.
        writeCharacteristic(cmdTx, frame, BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT).suspend()
    }
}
