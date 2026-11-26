Lapis-Deck seed folder (optional)

Nothing here is needed: on first boot the deck shows a Bluetooth PIN and the Lapis
companion app sets up Wi-Fi, Tailscale and your LLM keys.

To pre-configure a card instead, copy any of these files into this folder before the first
boot. They are imported once; secrets and keys are then deleted from this partition.

  lapis.toml          LLM provider / model / schedules -> /etc/lapis/lapis.toml
  hosts.toml          Tailscale hosts the agent may drive -> /etc/lapis/hosts.toml
  deck.toml           hardware settings, e.g. mesh_region = "US" (LoRa region) or the
                      AIO board revision -> /etc/lapis/deck.toml (start from the default)
  secrets.env         ANTHROPIC_API_KEY=... and friends -> /etc/lapis/secrets.env (0600)
  tailscale.authkey   a pre-auth key for a headless tailnet join
  lapis_ed25519       SSH private key for the fleet hosts (0600)
