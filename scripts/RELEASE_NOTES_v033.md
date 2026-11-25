## Lapis-Zero OS v0.3.3 — Claude Code token file + Gemini CLI JSON parsing

**CardputerZero** (M5Stack CM0, 512 MB RAM). Daemon-only release; `lapis-ui` is unchanged from 0.3.2.

### What changed
- **Claude Code on a Mac host** — its login lives in the login Keychain, which a session over SSH
  can't open, so chats ended "Not logged in · Please run /login". Each run now reads
  `~/.claude/lapis-token` (made with `claude setup-token`, `chmod 600`) into
  `CLAUDE_CODE_OAUTH_TOKEN` when it isn't already set. The token never leaves the host, and a
  `/login` reply now explains how to make one.
- **Gemini CLI** now runs with `-o json` and a new `read_gemini` parses its answer object past the
  `TERM=dumb` / "Ripgrep is not available" warnings (and no longer shows them as the ticker line).

### Checksum
| File | Bytes | SHA-256 |
|---|---|---|
| `lapis-zero-arm64.img.xz` | 641,771,016 | `547393d629daaa64cd384afdb328150097c3ba6be40450f3737fa59d57f75e25` |
| `lapis-zero-arm64.img` (extracted) | 5,368,709,120 | `81b3634c2573e8f7eae22c1ae5c1025ef727c4e81efb2b0840b52be382732456` |

### Flash it
```bash
# macOS (replace with your device, e.g. /dev/disk4)
xz -dc lapis-zero-arm64.img.xz | sudo dd of=/dev/rdisk4 bs=4m

# Linux
xz -dc lapis-zero-arm64.img.xz | sudo dd of=/dev/sdX bs=4M
```
Windows: use **BalenaEtcher** or **Rufus** and select the `.xz` directly (Etcher decompresses it).

### First boot
Insert the card, power on. The device brings up its own Tailscale KVM; find it with
`tailscale status` and open the lapis-ui or the Android app.

Verified: 285 tests pass, ruff clean, mypy unchanged.
