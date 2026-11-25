## Lapis-Zero v0.1.0 — first release

**Agentic firmware for the M5Stack CardputerZero** (Raspberry Pi CM0, 512 MB RAM).

A handheld that operates your computers for you: it looks at the screen over VNC,
clicks, types, runs commands over SSH, and asks before doing anything risky. There is
no cable between the handheld and the machine it controls.

### What's in this release

- **`lapis-zero-arm64.img.xz`** — the flashable SD-card image (Debian Trixie arm64 +
  Lapis-Zero). Decompress, then write to a microSD card (see below).
- **`app-debug.apk`** — Android companion app (BLE provisioning, chat relay, live
  screen frames). **This is a debug build signed for development** — install with
  `adb install app-debug.apk` or allow unknown sources on the phone.
- **`os-list.json`** — M5Stack Imager manifest for the OS image.

### Flash the image to a microSD card

The image is `.xz`-compressed. Decompress it, then write the raw image to the card.

**On a Mac (or Linux):**

```bash
# 1. Find your card (look for a removable disk ~32 GB)
diskutil list            # macOS
lsblk                    # Linux

# 2. Decompress (pick one)
xz -d lapis-zero-arm64.img.xz          # → lapis-zero-arm64.img
# or, to keep the .xz:
xz -dk lapis-zero-arm64.img.xz

# 3. Write to the card — replace /dev/rdisk4 (or /dev/sdX) with YOUR device.
#    WARNING: this erases everything on the card. Device name must be the raw disk,
#    not a partition (rdisk4, not rdisk4s1).
diskutil unmountDisk disk4                          # macOS: unmount first
sudo dd if=lapis-zero-arm64.img of=/dev/rdisk4 bs=4m   # macOS raw device
# Linux equivalent:
# sudo dd if=lapis-zero-arm64.img of=/dev/sdX bs=4M conv=fsync
```

**On Windows:** use [balenaEtcher](https://etcher.balena.io/) or
[Rufus](https://rufus.ie/) and select the `.img` file (decompress the `.xz` first with
7-Zip, or let Etcher handle it).

**On macOS you can also** use `balenaEtcher` or ApplePi-Baker for a GUI.

### First boot

1. Insert the card into the CardputerZero and power it on.
2. First boot resizes the root filesystem and runs the Lapis-Zero installer — it can
   take several minutes. The screen stays on the splash image; be patient.
3. Set up your agent host + Tailscale network either on the device (`Fn`-menus) or via
   the BLE pairing flow in the Android app.
4. See `os/docs/` and the repo `README.md` for the full setup and pairing guide.

### Verification

- Raw image: **4,571,791,360 bytes** (SHA-256 shown in `os-list.json`).
- `os-list.json` SHA-256 of the `.xz` matches this asset.

### Tags

Agentic firmware · CardputerZero · M5Stack · Rust · Python · Kotlin · VNC · SSH ·
Tailscale · LLM · KVM · Raspberry Pi CM0 · edge AI

---

*Apache-2.0. Early release — flash at your own risk; the device has shipped only
recently and this is one of the first custom firmwares for it.*
