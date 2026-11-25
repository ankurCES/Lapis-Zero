# Bluetooth start-up test against the real `bluetoothd`

Runs the image's own `bluetoothd` (BlueZ 5.82) and its own `lapis-ble-prov`
inside a container, with the kernel's Bluetooth management socket emulated, and
drives one complete pairing with a fake phone over D-Bus.

It exists because nothing else exercised this path: on the first hardware boot
`lapis-ble-prov` crashed 0.25 s after every start (the BlueZ library mistook
`/org/bluez/test`, exported by BlueZ's SAP plugin, for an adapter), and no unit
test or container boot could have shown it. Docker has no Bluetooth at all, so a
plain container boot only proves the "no adapter" path.

| File | Role |
| --- | --- |
| `fakebt.c` | `LD_PRELOAD` shim: maps `bluetoothd`'s `AF_BLUETOOTH` management socket onto a Unix socket |
| `mgmt_emu.py` | stand-in for the kernel management channel (one controller, `hci0`) |
| `fake_daemon.py` | stand-in for `lapis-daemon`'s Unix socket; prints what the service reports |
| `fake_phone.py` | speaks `lapis-ble-v1` to the exported GATT objects: handshake, PIN proof, scan, finish |
| `run-in-image.sh` | starts all of the above inside the image |

Run it against an exported image (the build does not do this for you; `gcc` is
in the image, nothing is installed into it):

```bash
docker run --rm -e PROBE_AT=5 -e PROBE_CMD='/opt/lapis/venv/bin/python /scratch/fake_phone.py' -v "$PWD/os/tests/ble-real-bluez:/scratch:ro" lapis-rootfs:final bash -c '/scratch/run-in-image.sh check 2 20'
```

Pass: `lapis-ble-prov: advertising as LapisZero-ABCD`, `bluetoothd` logs
`Advertisement registered`, the phone prints `OK at MTU 517`, and
`lapis-ble-prov exit=0`. The Wi-Fi scan step reports an error there, which is
expected: a container has no Wi-Fi.

What it cannot show: the advertisement on air, real ATT traffic from a phone,
or the controller's behaviour. Only the device can.
