"""Userspace stand-in for the kernel Bluetooth mgmt channel (one LE+BR/EDR controller, legacy adv).

Test rig for running the REAL bluetoothd 5.82 offline. It answers mgmt commands the way
net/bluetooth/mgmt.c does for the cases exercised here; anything unknown gets
"Unknown Command" and is logged. Knobs (env):
  EMU_INDEX_DELAY   seconds before hci0 is announced (Index Added)        default 0
  EMU_POWER_DELAY   seconds the kernel takes to complete Set Powered      default 0.2
  EMU_RFKILL        1 = Set Powered(on) fails with RFKilled until /tmp/emu-unblock exists
  EMU_POWERED       1 = controller already powered at start               default 0
"""
import heapq, os, selectors, socket, struct, sys, time

T0 = time.monotonic()
def log(*a):
    print(f"[kernel-emu {time.monotonic()-T0:6.2f}]", *a, flush=True)

PATH = os.environ.get("FAKEBT_MGMT", "/tmp/fake-mgmt.sock")
INDEX_DELAY = float(os.environ.get("EMU_INDEX_DELAY", "0"))
POWER_DELAY = float(os.environ.get("EMU_POWER_DELAY", "0.2"))
RFKILL = os.environ.get("EMU_RFKILL") == "1"

S_POWERED, S_CONNECTABLE, S_FAST, S_DISCOV, S_BONDABLE, S_LINKSEC, S_SSP, S_BREDR = (1 << i for i in range(8))
S_HS, S_LE, S_ADV, S_SC, S_DEBUG, S_PRIVACY, S_CONFIG, S_STATIC = (1 << i for i in range(8, 16))
SUPPORTED = (S_POWERED | S_CONNECTABLE | S_FAST | S_DISCOV | S_BONDABLE | S_LINKSEC | S_SSP | S_BREDR
             | S_LE | S_ADV | S_SC | S_DEBUG | S_PRIVACY | S_STATIC)
state = {"settings": S_SSP | S_BREDR | S_LE | S_SC | (S_POWERED if os.environ.get("EMU_POWERED") == "1" else 0),
         "name": b"raspberrypi", "class": b"\0\0\0", "adv": {}}
MODE_BITS = {0x05: S_POWERED, 0x06: S_DISCOV, 0x07: S_CONNECTABLE, 0x08: S_FAST, 0x09: S_BONDABLE,
             0x0A: S_LINKSEC, 0x0B: S_SSP, 0x0C: S_HS, 0x0D: S_LE, 0x29: S_ADV, 0x2A: S_BREDR,
             0x2D: S_SC, 0x2E: S_DEBUG, 0x2F: S_PRIVACY}
OPNAME = {0x05: "Set Powered", 0x0D: "Set LE", 0x0F: "Set Local Name", 0x3D: "Read Adv Features",
          0x3E: "Add Advertising", 0x3F: "Remove Advertising", 0x54: "Add Ext Adv Params",
          0x55: "Add Ext Adv Data", 0x04: "Read Info", 0x03: "Read Index List"}
ST_OK, ST_UNKNOWN, ST_INVALID, ST_BUSY, ST_REJECTED, ST_RFKILLED = 0x00, 0x01, 0x0D, 0x0A, 0x0B, 0x12
# mgmt status codes: 0x0A Busy, 0x0B Rejected, 0x0C Not Supported, 0x0D Invalid Params, 0x12 RFKilled
ADV_FLAGS_SUPPORTED = 0x1F07F  # connectable..local-name, tx-power, duration/timeout/intervals/tx/scan-rsp params

clients, timers = [], []
sel = selectors.DefaultSelector()

def send(conn, event, index, payload=b""):
    try:
        conn.send(struct.pack("<HHH", event, index, len(payload)) + payload)
    except OSError as exc:
        log("send failed", exc)

def complete(conn, op, index, data=b"", status=ST_OK):
    send(conn, 0x0001, index, struct.pack("<HB", op, status) + data)

def status(conn, op, index, st):
    send(conn, 0x0002, index, struct.pack("<HB", op, st))

def broadcast(event, index, payload=b"", skip=None):
    for c in clients:
        if c is not skip:
            send(c, event, index, payload)

def later(delay, fn):
    heapq.heappush(timers, (time.monotonic() + delay, id(fn), fn))

def index_present():
    return time.monotonic() - T0 >= INDEX_DELAY

def info():
    return (bytes.fromhex("cdab12eb27b8") + bytes([0x08]) + struct.pack("<H", 0x000F)
            + struct.pack("<II", SUPPORTED, state["settings"]) + state["class"]
            + state["name"].ljust(249, b"\0") + b"".ljust(11, b"\0"))

def ad_fields(data):
    out, i = [], 0
    while i < len(data) and data[i]:
        ln, typ = data[i], data[i + 1]
        out.append(f"len={ln:2d} type=0x{typ:02x} value={data[i+2:i+1+ln].hex()}")
        i += ln + 1
    return out

def handle(conn, op, index, p):
    name = OPNAME.get(op, f"0x{op:04x}")
    if op == 0x01:
        return complete(conn, op, index, struct.pack("<BH", 1, 23))
    if op == 0x02:
        cmds, evts = list(range(1, 0x5B)), list(range(1, 0x32))
        return complete(conn, op, index, struct.pack("<HH", len(cmds), len(evts))
                        + b"".join(struct.pack("<H", x) for x in cmds + evts))
    if op == 0x03:
        ids = [0] if index_present() else []
        log(name, "->", ids)
        return complete(conn, op, index, struct.pack("<H", len(ids)) + b"".join(struct.pack("<H", i) for i in ids))
    if op == 0x36:
        return complete(conn, op, index, struct.pack("<H", 0))
    if op == 0x3C:
        ids = [0] if index_present() else []
        return complete(conn, op, index, struct.pack("<H", len(ids)) + b"".join(struct.pack("<HBB", i, 0, 3) for i in ids))
    if index != 0 or not index_present():
        return status(conn, op, index, 0x11)  # Invalid Index
    if op == 0x04:
        log(name, f"settings=0x{state['settings']:08x}")
        return complete(conn, op, index, info())
    if op in MODE_BITS:
        bit, val = MODE_BITS[op], p[0]
        if op == 0x05:
            blocked = RFKILL and not os.path.exists("/tmp/emu-unblock")
            log(f"Set Powered({val}) requested; rfkill-blocked={blocked}")
            if val and blocked:
                return status(conn, op, index, ST_RFKILLED)
            def done():
                state["settings"] = (state["settings"] | bit) if val else (state["settings"] & ~bit)
                log(f"Set Powered({val}) complete; settings=0x{state['settings']:08x}")
                complete(conn, op, index, struct.pack("<I", state["settings"]))
                broadcast(0x0006, 0, struct.pack("<I", state["settings"]), skip=conn)
            return later(POWER_DELAY if val else 0, done)
        state["settings"] = (state["settings"] | bit) if val else (state["settings"] & ~bit)
        complete(conn, op, index, struct.pack("<I", state["settings"]))
        return broadcast(0x0006, 0, struct.pack("<I", state["settings"]), skip=conn)
    if op in (0x0E, 0x10, 0x11):
        return complete(conn, op, index, state["class"])
    if op == 0x0F:
        state["name"] = p[:249].split(b"\0")[0]
        log("Set Local Name ->", state["name"].decode(errors="replace"))
        return complete(conn, op, index, p[:260].ljust(260, b"\0"))
    if op in (0x12, 0x13, 0x18, 0x28, 0x2C, 0x30, 0x35, 0x43, 0x46, 0x4C, 0x4E, 0x2B):
        return complete(conn, op, index, struct.pack("<I", state["settings"]) if op == 0x2B else b"")
    if op == 0x15:
        return complete(conn, op, index, struct.pack("<H", 0))
    if op in (0x26, 0x27, 0x33, 0x34):
        return complete(conn, op, index, p[:7].ljust(7, b"\0"))
    if op == 0x3D:
        inst = sorted(state["adv"])
        return complete(conn, op, index, struct.pack("<IBBBB", ADV_FLAGS_SUPPORTED, 31, 31, 5, len(inst)) + bytes(inst))
    if op == 0x3F:
        state["adv"].pop(p[0], None) if p[0] else state["adv"].clear()
        log("Remove Advertising instance", p[0])
        return complete(conn, op, index, p[:1])
    if op == 0x44:
        return complete(conn, op, index, struct.pack("<III", 0x1FF, 0x1FE, 0x1FF) if False else struct.pack("<III", 0x7, 0x6, 0x7))
    if op == 0x48:
        return complete(conn, op, index, struct.pack("<H", 0))
    if op == 0x49:
        return complete(conn, op, index, struct.pack("<H", 0))
    if op in (0x4B, 0x4D):
        return complete(conn, op, index, b"")
    if op == 0x51:
        return complete(conn, op, index, struct.pack("<IIHBH", 0, 0, 0, 0, 0))
    if op == 0x54:
        inst, flags, duration, timeout, mn, mx, txp = struct.unpack("<BIHHIIb", p[:18])
        log(f"Add Ext Adv Params instance={inst} flags=0x{flags:08x} duration={duration} timeout={timeout} "
            f"powered={bool(state['settings'] & S_POWERED)}")
        if flags & ~ADV_FLAGS_SUPPORTED & 0xFFFF_FFFF:
            log("  -> Invalid Parameters (flag not supported by a legacy controller, e.g. secondary PHY)")
            return status(conn, op, index, ST_INVALID)
        if not state["settings"] & S_POWERED:
            log("  -> Rejected (mgmt.c: 'In new interface, we require that we are powered to register')")
            return status(conn, op, index, ST_REJECTED)
        state["adv"][inst] = {"flags": flags}
        return complete(conn, op, index, struct.pack("<BbBB", inst, 0x7F, 31, 31))
    if op == 0x55:
        inst, alen, slen = p[0], p[1], p[2]
        adv, scan = p[3:3 + alen], p[3 + alen:3 + alen + slen]
        log(f"Add Ext Adv Data instance={inst} adv_data_len={alen} scan_rsp_len={slen}")
        log("  ADV  bytes:", adv.hex()); [log("    ", f) for f in ad_fields(adv)]
        log("  SCAN bytes:", scan.hex()); [log("    ", f) for f in ad_fields(scan)]
        if alen > 31 or slen > 31:
            return status(conn, op, index, ST_INVALID)
        if not state["settings"] & S_POWERED:
            return status(conn, op, index, ST_REJECTED)
        return complete(conn, op, index, bytes([inst]))
    if op == 0x3E:
        inst, flags, duration, timeout, alen, slen = struct.unpack("<BIHHBB", p[:11])
        log(f"Add Advertising (legacy cmd) instance={inst} flags=0x{flags:08x} adv={p[11:11+alen].hex()} scan={p[11+alen:11+alen+slen].hex()}")
        return complete(conn, op, index, bytes([inst]))
    log(f"UNHANDLED opcode {name} index={index} params={p.hex()} -> Unknown Command")
    status(conn, op, index, ST_UNKNOWN)

def on_readable(conn):
    try:
        pkt = conn.recv(4096)
    except OSError:
        pkt = b""
    if not pkt:
        sel.unregister(conn); clients.remove(conn); conn.close(); log("mgmt client closed"); return
    op, index, ln = struct.unpack("<HHH", pkt[:6])
    handle(conn, op, index, pkt[6:6 + ln])

def main():
    if os.path.exists(PATH):
        os.unlink(PATH)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    srv.bind(PATH); srv.listen(8)
    sel.register(srv, selectors.EVENT_READ, "accept")
    log(f"mgmt emulator on {PATH}; index_delay={INDEX_DELAY} power_delay={POWER_DELAY} rfkill={RFKILL}")
    announced = INDEX_DELAY == 0
    while True:
        timeout = 0.05
        for key, _ in sel.select(timeout):
            if key.data == "accept":
                c, _ = srv.accept(); clients.append(c); sel.register(c, selectors.EVENT_READ, "client"); log("mgmt client connected")
            else:
                on_readable(key.fileobj)
        now = time.monotonic()
        while timers and timers[0][0] <= now:
            heapq.heappop(timers)[2]()
        if not announced and index_present():
            announced = True
            log("Index Added hci0")
            broadcast(0x0004, 0)

main()
