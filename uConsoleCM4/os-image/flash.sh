#!/usr/bin/env bash
# Writes a Lapis-Deck image to an SD card and verifies it, partition by partition.
#
#   os-image/flash.sh /dev/disk6 [os-image/deploy/lapis-deck-cm4-trixie-arm64.img.xz]
#
# macOS: refuses internal disks, asks you to type the disk name, writes the raw device
# (/dev/rdiskN) with sudo (your password is asked by sudo itself, in your terminal), then
# compares the root filesystem byte for byte and the boot partition file by file (macOS
# automounts bootfs and writes .Spotlight-V100/.fseventsd into it, so a whole-card hash can
# never match). Linux: same write; the rootfs is compared, the boot partition by size.
set -euo pipefail

DISK="${1:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMG="${2:-${HERE}/deploy/lapis-deck-cm4-trixie-arm64.img.xz}"

die() {
    echo "flash: $*" >&2
    exit 1
}

[ -n "${DISK}" ] || die "usage: $0 /dev/diskN [image.img.xz]"
[ -f "${IMG}" ] || die "no image at ${IMG} (build it with os-image/build.sh)"
if [ -f "${IMG}.sha256" ]; then
    (cd "$(dirname "${IMG}")" && shasum -a 256 -c "$(basename "${IMG}").sha256") || die "image checksum mismatch"
fi

OS="$(uname -s)"
if [ "${OS}" = "Darwin" ]; then
    info="$(diskutil info "${DISK}")" || die "${DISK} is not a disk"
    echo "${info}" | grep -Eq 'Device Location:[[:space:]]+External|Removable Media:[[:space:]]+(Removable|Yes)' ||
        die "${DISK} does not look like a removable card; refusing"
    echo "${info}" | grep -E 'Device / Media Name|Disk Size' || true
    RAW="${DISK/\/dev\/disk//dev/rdisk}"
    PART() { echo "${DISK}s$1"; }
else
    [ -b "${DISK}" ] || die "${DISK} is not a block device"
    [ "$(lsblk -dno RM "${DISK}")" = "1" ] || die "${DISK} is not removable; refusing"
    lsblk "${DISK}"
    RAW="${DISK}"
    PART() { case "${DISK}" in *[0-9]) echo "${DISK}p$1" ;; *) echo "${DISK}$1" ;; esac; }
fi

read -r -p "This ERASES ${DISK}. Type its name (${DISK##*/}) to continue: " answer
[ "${answer}" = "${DISK##*/}" ] || die "not confirmed"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
RAWIMG="${WORK}/image.img"
echo "Decompressing ..."
xz -dc "${IMG}" > "${RAWIMG}"

if [ "${OS}" = "Darwin" ]; then diskutil unmountDisk "${DISK}"; else sudo umount "$(PART 1)" "$(PART 2)" 2>/dev/null || true; fi
echo "Writing $(du -h "${RAWIMG}" | cut -f1) (sudo asks for your password) ..."
if [ "${OS}" = "Darwin" ]; then
    sudo dd if="${RAWIMG}" of="${RAW}" bs=4m status=progress
else
    sudo dd if="${RAWIMG}" of="${RAW}" bs=4M conv=fsync status=progress
fi
sync

# MBR partition table of the image: (start sector, sector count) of partitions 1 and 2.
read -r P1S P1N P2S P2N < <(python3 - "${RAWIMG}" <<'PY'
import struct, sys
mbr = open(sys.argv[1], "rb").read(512)
parts = [struct.unpack_from("<II", mbr, 446 + 16 * i + 8) for i in range(2)]
print(parts[0][0], parts[0][1], parts[1][0], parts[1][1])
PY
)

# Read in 1 MiB blocks when the partition is MiB-aligned (pi-gen's are): 512-byte reads of a
# raw card take many minutes.
if [ $((P2S % 2048)) -eq 0 ] && [ $((P2N % 2048)) -eq 0 ]; then
    BS=1048576; SKIP=$((P2S / 2048)); COUNT=$((P2N / 2048))
else
    BS=512; SKIP=${P2S}; COUNT=${P2N}
fi
echo "Verifying the root filesystem (partition 2, $((P2N / 2048)) MiB) ..."
want=$(dd if="${RAWIMG}" bs="${BS}" skip="${SKIP}" count="${COUNT}" 2>/dev/null | shasum -a 256 | cut -d' ' -f1)
if [ "${OS}" = "Darwin" ]; then diskutil unmountDisk "${DISK}" >/dev/null 2>&1 || true; fi
got=$(sudo dd if="${RAW}" bs="${BS}" skip="${SKIP}" count="${COUNT}" 2>/dev/null | shasum -a 256 | cut -d' ' -f1)
[ "${want}" = "${got}" ] || die "rootfs differs from the image (${got} != ${want})"
echo "  rootfs OK (${want})"

echo "Verifying the boot partition file by file ..."
if [ "${OS}" = "Darwin" ]; then
    dev=$(hdiutil attach -readonly -nomount -imagekey diskimage-class=CRawDiskImage "${RAWIMG}" | awk 'NR==1{print $1}')
    trap 'hdiutil detach "${dev}" >/dev/null 2>&1 || true; rm -rf "${WORK}"' EXIT
    mkdir -p "${WORK}/img-boot" "${WORK}/card-boot"
    diskutil mount readOnly -mountPoint "${WORK}/img-boot" "${dev}s1" >/dev/null
    diskutil mount readOnly -mountPoint "${WORK}/card-boot" "$(PART 1)" >/dev/null
    diff -rq -x .Spotlight-V100 -x .fseventsd -x '._*' -x .Trashes "${WORK}/img-boot" "${WORK}/card-boot" || die "boot partition differs"
    diskutil unmount "${WORK}/img-boot" >/dev/null
    diskutil unmount "${WORK}/card-boot" >/dev/null
    echo "  bootfs OK"
    diskutil eject "${DISK}"
else
    [ "$(sudo blockdev --getsz "$(PART 1)")" -ge "${P1N}" ] || die "boot partition too small"
    echo "  bootfs size OK (${P1N} sectors at ${P1S})"
fi
echo "Done: put the card in the uConsole and power on. First boot shows a Bluetooth PIN."
