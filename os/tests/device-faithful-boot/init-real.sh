#!/bin/sh
# Boot the image in a container the way the device boots it:
#  * / is read-only when PID 1 starts (kernel/initramfs mount it ro; systemd-remount-fs
#    makes it rw from fstab), so systemd's first-boot preset pass cannot write /etc;
#  * PID 1 listens to udev (Docker's /dev is not devtmpfs, which disables its monitor
#    unless /run/udev/control exists), so device-triggered transactions happen.
mkdir -p /run/udev && : > /run/udev/control
mount -o remount,ro / || echo "init-real: remount ro failed" > /dev/kmsg
exec /sbin/init "$@"
