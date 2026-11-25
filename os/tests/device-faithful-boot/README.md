# Booting the image in a container the way the device boots it

A plain `docker run ... /sbin/init` of the image is not evidence for the device:

* with a writable root when PID 1 starts, systemd's first-boot preset pass
  enables units the device never enables (on the device `/` is still read-only
  at that point);
* Docker's `/dev` is not devtmpfs, which switches off PID 1's udev monitor, so
  device-triggered transactions (the ones that restarted the BSP splash) never
  happen;
* units with `ConditionVirtualization=!container` (the BSP's `fb_load`) are
  skipped, and there is no framebuffer, rfkill or Bluetooth at all.

`init-real.sh` fixes the first two. Export the built image to a Docker image
(`docker import` of its root filesystem with `/boot/firmware` mounted in), then:

```bash
docker run -d --name lapis-sim --privileged --cgroupns=host -e container=docker --tmpfs /run --tmpfs /run/lock --tmpfs /tmp:exec -v /sys/fs/cgroup:/sys/fs/cgroup:rw -v "$PWD/os/tests/device-faithful-boot/fstab:/etc/fstab:ro" -v "$PWD/os/tests/device-faithful-boot/init-real.sh:/init-real.sh:ro" lapis-rootfs:final /init-real.sh
```

The boot sits about 90 s behind `dev-zram0.device`, which never appears in a
container. For Bluetooth use `../ble-real-bluez/`. The display handover, the
radios and the keypad can only be shown by the device; its
`lapis-boot-report.txt` is the evidence.
