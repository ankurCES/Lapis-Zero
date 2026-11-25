Lapis-Zero OS seed directory
============================

Optional. Copy any of these files into this folder before the first boot and
they are imported into /etc/lapis/ automatically:

  lapis.toml          LLM provider / model / schedules  (see config/lapis.example.toml)
  hosts.toml          Tailscale target machines          (see config/hosts.example.toml)
  secrets.env         ANTHROPIC_API_KEY=..., LAPIS_VNC_...=...
  tailscale.authkey   Tailscale pre-auth key for a headless tailnet join
  lapis_ed25519       SSH private key used to reach target hosts

secrets.env, tailscale.authkey and lapis_ed25519 are deleted from this
partition as soon as they have been imported.

Without any seed files, pair the Lapis-Zero Android app over Bluetooth: the
device shows a 6-digit PIN on its screen on first boot.

Troubleshooting: ../lapis-boot-report.txt is rewritten 45 seconds after every
boot, several more times in the first half hour, and whenever a Bluetooth
pairing session ends, with the state of the Lapis services, display and
network. Leave the device on for three minutes before pulling the card.

The user-data, meta-data and network-config files in the root of this partition
are cloud-init's. cloud-init is switched off in Lapis-Zero OS, so editing them
has no effect, and neither has OS customisation written in that format by the
Raspberry Pi Imager. Wi-Fi is set up from the Android app over Bluetooth.
