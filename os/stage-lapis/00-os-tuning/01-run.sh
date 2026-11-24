#!/bin/bash -e
# Lapis-Zero OS hardening: kernel/cmdline, zram, storage, journald.
# Runs with ROOTFS_DIR pointing at the stage rootfs (not chrooted), per pi-gen
# convention for config.txt / cmdline.txt / fstab / etc edits.

# -- /boot/firmware/config.txt: gpu_mem=16, keep all CardputerZero overlays --
CFG="${ROOTFS_DIR}/boot/firmware/config.txt"
if [ -f "${CFG}" ]; then
    # Reserve only 16 MiB for the VC4/GPU; keep every BSP overlay
    # (st7789, tca8418, es8389, bmi270, bq27220, rx8130ce, cardputerzero)
    # untouched.
    if grep -q '^gpu_mem=' "${CFG}"; then
        sed -i 's/^gpu_mem=.*/gpu_mem=16/' "${CFG}"
    else
        printf 'gpu_mem=16\n' >> "${CFG}"
    fi
    # Keep the BSP's monochrome splash rather than the stock one.
    sed -i 's/^splash_screen=.*/splash_screen=CardputerZero_splash_custom.bin/' "${CFG}"
fi

# -- /boot/firmware/cmdline.txt: keep kernel text off the framebuffer --
CMDLINE="${ROOTFS_DIR}/boot/firmware/cmdline.txt"
if [ -f "${CMDLINE}" ]; then
    for token in fbcon=map:off quiet loglevel=3 systemd.show_status=false; do
        if ! grep -q "${token}" "${CMDLINE}"; then
            sed -i "1s/$/ ${token}/" "${CMDLINE}"
        fi
    done
fi

# -- zram-generator: 512 MB LZ4 swap (min(ram,512)) --
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd"
install -m 0644 "${STAGE_DIR}/files/zram-generator.conf" \
    "${ROOTFS_DIR}/etc/systemd/zram-generator.conf"

# -- fstab: noatime on /, /tmp as tmpfs --
if [ -f "${ROOTFS_DIR}/etc/fstab" ]; then
    # Append noatime to the / ext4 line without duplicating the option.
    if ! grep -E '^[^#].*[[:space:]]/[[:space:]]' "${ROOTFS_DIR}/etc/fstab" | grep -q noatime; then
        sed -i -E '/^[^#].*[[:space:]]\/[[:space:]]/ s/([[:space:]]+)ext4([[:space:]]+)([^[:space:]]+)/\1ext4\2\3,noatime/' \
            "${ROOTFS_DIR}/etc/fstab"
    fi
fi
if ! grep -q '[[:space:]]/tmp[[:space:]]' "${ROOTFS_DIR}/etc/fstab"; then
    cat >> "${ROOTFS_DIR}/etc/fstab" <<'EOF'
tmpfs   /tmp   tmpfs   defaults,size=64M,noatime,nosuid,nodev   0 0
EOF
fi

# -- volatile journal --
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/journald.conf.d"
cat > "${ROOTFS_DIR}/etc/systemd/journald.conf.d/volatile.conf" <<'EOF'
[Journal]
Storage=volatile
RuntimeMaxUse=16M
EOF
