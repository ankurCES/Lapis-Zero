"""A 'phone' that speaks lapis-ble-v1 to the exported GATT objects over D-Bus, calling
ReadValue/WriteValue/StartNotify with the option dictionaries bluetoothd passes
(doc/org.bluez.GattCharacteristic.rst). It exercises gatt_server.py + session.py end to end;
it does NOT exercise BlueZ's ATT layer or the radio."""
import asyncio, json, os, sys
from dbus_next import BusType, Message, MessageType, Variant
from dbus_next.aio import MessageBus
from lapis_daemon.ble_prov import crypto

BASE = "/com/spacecheese/bluez_peripheral"
MTU = int(os.environ.get("PHONE_MTU", "517"))
DEV = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF"

async def main():
    bus = await MessageBus(bus_type=BusType.SYSTEM).connect()
    names = (await bus.call(Message(destination="org.freedesktop.DBus", path="/org/freedesktop/DBus",
             interface="org.freedesktop.DBus", member="ListNames"))).body[0]
    owner = objs = None
    for n in names:
        if not n.startswith(":") or n == bus.unique_name:
            continue
        r = await bus.call(Message(destination=n, path=BASE, interface="org.freedesktop.DBus.ObjectManager", member="GetManagedObjects"))
        if r.message_type == MessageType.METHOD_RETURN and r.body[0]:
            owner, objs = n, r.body[0]; break
    assert owner, "no GATT application found on the bus"
    by_uuid = {v["org.bluez.GattCharacteristic1"]["UUID"].value[-4:]: p for p, v in objs.items() if "org.bluez.GattCharacteristic1" in v}
    hs, tx, rx = by_uuid["0002"], by_uuid["0003"], by_uuid["0004"]
    print("phone: app owner", owner, "handshake", hs[-5:], "cmd_tx", tx[-5:], "evt_rx", rx[-5:], flush=True)

    async def call(path, member, sig, body):
        r = await bus.call(Message(destination=owner, path=path, interface="org.bluez.GattCharacteristic1", member=member, signature=sig, body=body))
        if r.message_type == MessageType.ERROR:
            raise RuntimeError(f"{member} -> {r.error_name}: {r.body}")
        return r.body

    ropts = {"offset": Variant("q", 0), "mtu": Variant("q", MTU), "device": Variant("o", DEV), "link": Variant("s", "LE")}
    wopts = lambda t: {"offset": Variant("q", 0), "type": Variant("s", t), "mtu": Variant("q", MTU), "device": Variant("o", DEV), "link": Variant("s", "LE")}

    notes: asyncio.Queue = asyncio.Queue()
    def on_msg(m):
        if m.message_type == MessageType.SIGNAL and m.member == "PropertiesChanged" and m.path == rx and "Value" in m.body[1]:
            notes.put_nowait(bytes(m.body[1]["Value"].value))
    bus.add_message_handler(on_msg)
    await bus.call(Message(destination="org.freedesktop.DBus", path="/org/freedesktop/DBus", interface="org.freedesktop.DBus",
                           member="AddMatch", signature="s", body=[f"type='signal',sender='{owner}',path='{rx}'"]))

    pin = open("/tmp/pin").read().strip()  # read before the handshake: the rig rewrites it on every status
    hello = bytes((await call(hs, "ReadValue", "a{sv}", [ropts]))[0])
    # long read continuation as BlueZ does it at small MTU (Read Blob -> ReadValue with offset)
    part = bytes((await call(hs, "ReadValue", "a{sv}", [dict(ropts, offset=Variant("q", 22))]))[0])
    assert part == hello[22:], "offset read mismatch"
    dev_pub, dev_nonce = crypto.parse_hello(hello)
    priv, pub = crypto.generate_keypair()
    nonce = os.urandom(16)
    await call(hs, "WriteValue", "aya{sv}", [crypto.build_hello(pub, nonce), wopts("request")])
    keys = crypto.derive_keys(crypto.compute_shared(priv, dev_pub), pub, dev_pub, nonce, dev_nonce, pin)
    await call(rx, "StartNotify", "", [])
    seq = {"tx": 0, "rx": 0, "msg": 0}
    reasm = crypto.Reassembler()

    async def cmd(obj):
        seq["tx"] += 1
        env = crypto.seal(keys.k_c2d, crypto.DIR_C2D, seq["tx"], json.dumps(obj).encode())
        for f in crypto.split_frames(seq["msg"], crypto.FLAG_ENCRYPTED, env, crypto.max_chunk_data(MTU)):
            await call(tx, "WriteValue", "aya{sv}", [f, wopts("command")])
        seq["msg"] = (seq["msg"] + 1) % 256

    async def evt(timeout=8):
        while True:
            done = reasm.feed(await asyncio.wait_for(notes.get(), timeout))
            if done:
                seq["rx"] += 1
                return json.loads(crypto.open_envelope(keys.k_d2c, crypto.DIR_D2C, done[1], seq["rx"]))

    await cmd({"cmd": "pin_verify", "proof": crypto.client_proof(keys).hex(), "device_name": "Pixel", "device_model": "rig"})
    e = await evt(); assert e["evt"] == "pin_verified" and bytes.fromhex(e["proof"]) == crypto.server_proof(keys), e
    print("phone: pin_verified, device_id", e["device_id"], "token chars", len(e["api_token"]), flush=True)
    await cmd({"cmd": "wifi_scan"}); e = await evt(40); print("phone:", e["evt"], str(e)[:100], flush=True)
    await cmd({"cmd": "finish_provisioning"}); e = await evt(); print("phone:", e, flush=True)
    print(f"phone: OK at MTU {MTU}", flush=True)

asyncio.run(main())
