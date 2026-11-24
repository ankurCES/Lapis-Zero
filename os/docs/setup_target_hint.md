# Binding a target host's VNC/SSH strictly to its Tailscale IP

Lapis-Zero's Option C KVM reaches target machines **only over Tailscale**
(`100.x.y.z` addresses). For security, a target's VNC/SSH listener must never
be exposed on the public/LAN interface — bind it to the Tailscale interface
only.

## macOS target (built-in Screen Sharing / VNC)

1. Install Tailscale and note the machine's tailnet IP: `tailscale ip -4`.
2. System Settings → General → Sharing → Screen Sharing → Computer Settings…
   → enable "VNC viewers may control screen with password", set a strong
   password.
3. The macOS Screen Sharing server (`screensharingd`) listens on all
   interfaces; VNC itself offers no per-interface bind. Restrict it with the
   built-in firewall:
   - System Settings → Network → Firewall → Options, allow **only** incoming
     connections on `Remote Management`/`Screen Sharing` from the tailnet
     range (e.g. `100.64.0.0/10`).
   - Leave `Stealth mode` on so non-tailnet probes stay silent.
4. Record the VNC password in `hosts.toml` (per-host `vnc.password`) and keep
   the connection inside the tailnet via `tailscale up --ssh`.

## Linux target (TigerVNC / x11vnc)

Bind the server explicitly to the Tailscale interface:

```sh
# TigerVNC
x0vncserver -SecurityTypes VncAuth -passwordfile ~/.vnc/passwd \
    -localhost no -interface 100.64.0.0/10   # does not take a CIDR literally
```

`x11vnc` supports a literal bind address:

```sh
x11vnc -listen "$(tailscale ip -4)" -rfbauth ~/.vnc/passwd -forever
```

For `tigervnc` the equivalent is listening on the tailscale IP directly:

```sh
vncserver -localhost=0 -interface "$(tailscale ip -4)" :1
```

Then configure `hosts.toml`:

```toml
[[hosts]]
hostname = "linux-host"
tailscale_ip = "100.64.0.2"
vnc = { port = 5901, password_env = "LAPIS_VNC_PASSWORD_linux_host" }
ssh = { port = 22, user = "user", auth = "key", key_path = "/etc/lapis/lapis_ed25519" }
```

## SSH

`tailscale up --ssh` on the target already exposes SSH only on the tailnet.
For host keys, point `hosts.toml` at `/etc/lapis/lapis_ed25519` (see the
first-boot seed importer).

## Verification

After joining both devices to the tailnet, confirm the target is reachable
only via its tailscale IP and unreachable from the LAN:

```sh
lapis-daemon --mock   # once: smoke the KVM loop without a real host
tailscale ping <host>
nc -z <tailscale_ip> <port>   # should succeed only on tailnet path
```
