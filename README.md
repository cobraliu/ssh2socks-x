# ssh2socks

English | [简体中文](README.zh-CN.md)

A small desktop app for SSH tunnels built on your `~/.ssh/config`: one click starts a SOCKS5 proxy (`ssh -D`), a local port forward (`ssh -L`) or a remote port forward (`ssh -R`). It can also edit your SSH config and manage your keys.
Written in Rust with Tauri v2. Installers and binaries are a few MB. Runs on Windows, macOS and Linux.

## Features

- Reads hosts from `~/.ssh/config`, including `Include` files, with search
- Three tunnel types:
  - **SOCKS5 proxy**: opens a SOCKS5 proxy on this machine; traffic leaves through the server
  - **Local forward**: maps a port on the server, or on the server's LAN, to `127.0.0.1:<port>` on this machine
  - **Remote forward**: publishes a local port on the server's `0.0.0.0:<port>` so others can reach it
  - Both forward types have an "Open" button that opens the http address in your browser
- New servers' host keys are trusted automatically on first connect (`StrictHostKeyChecking=accept-new`), so it never hangs on a yes/no prompt. A changed key for a known host is still refused, with a hint on how to fix it
- Start or stop tunnels one at a time, or all at once
- Reconnects automatically (exponential backoff from 1s to 30s), with `ServerAliveInterval` keepalives
- Health check every 30 seconds:
  - SOCKS: fetches the probe URL through the proxy. An `http://` URL gets a full HTTP request; for `https://` a successful SOCKS CONNECT counts as up (no TLS handshake)
  - Local forward: checks whether a forwarded connection is dropped right away (which happens when the server can't reach the target)
  - Remote forward: checks that the local service is running, then connects to `server:port` from this machine
- Errors show up right away: every log line is timestamped, the list shows the latest error, and a connection that isn't up after 10 seconds is noted in the log. After 45 seconds the attempt is abandoned and retried
- Shows where a connection is stuck: the log names the step (TCP connect, SSH version exchange, key exchange, login, or reaching the target through the jump host), which host it is waiting on (jump host or server), and likely causes. Connections that fail also record how far they got, and the ssh version is logged
- **SSH config editor** ("SSH config" tab):
  - View, add, edit and delete hosts in `~/.ssh/config` (alias, hostname, user, port, identity file)
  - Connect directly, through a jump host (`ProxyJump`), or with a `ProxyCommand`. Templates cover a jump host, a SOCKS5 proxy, an HTTP proxy and ncat
  - Only the edited host's fields are changed. Comments, other options, `Include`/`Match` blocks and line endings are left as they are. The file is backed up to `config.ssh2socks.bak` before each save
  - New hosts are inserted before `Host *` / `Match` so they take effect
  - "Test connection" checks that key-based login works. The config file can also be opened in your system editor
- **Key management** ("Keys" tab):
  - Lists `~/.ssh/*.pub` with type, SHA256 fingerprint and comment. View or copy a public key in one click
  - Generate key pairs: Ed25519 or RSA (2048 / 3072 / 4096), with your own file name and comment and an optional passphrase. `ssh-keygen` is not needed
  - Import existing key pairs from files or pasted text. OpenSSH format and RSA PEM (PKCS#1 / PKCS#8) are supported; encrypted private keys need their passphrase
  - Each import is checked step by step, and the result of each step is shown: parse the private key → decrypt it with the passphrase → match it to the public key → sign with the private key and verify with the public key → encrypt with the public key and decrypt with the private key (RSA uses OAEP; Ed25519 is mapped to X25519 for a key exchange)
  - No duplicates: saving is refused if the file name is taken, or if the same key (same fingerprint) already exists under another name. Existing files are never overwritten
  - Private keys are saved with mode `600` (Linux / macOS)
- English and Chinese interface, including error messages, logs and the tray menu. English by default; switch with the "EN / 中" button at the top right
- Light and dark themes: follow the system, or pick one with the theme button (◐ / ☀ / ☾) at the top right. Both choices are remembered
- Lives in the system tray; closing the window keeps tunnels running
- Each tunnel's ssh runs in its own Job Object (Windows) or process group (Linux / macOS). Stopping or retrying a tunnel also ends its `ProxyJump` / `ProxyCommand` helper processes, and everything is cleaned up on exit
- If the app itself is killed or crashes: on Windows the Job Object still ends every ssh; on Linux / macOS, `SIGTERM` / `SIGINT` / `SIGHUP` are handled like Quit, and on the next launch any ssh left behind is stopped. It is only stopped when its pid, exact start time, name and boot all match what was recorded, so an unrelated process that reused the pid is never touched

## Requirements

- An OpenSSH client (`ssh` on PATH). Windows 10/11 include one by default
- **Key-based login**. Tunnels run with `BatchMode=yes`, so there is no password prompt. For passphrase-protected keys, add them to ssh-agent with `ssh-add` first
- Windows: the WebView2 runtime (bundled with Windows 11; the installer downloads it if needed)
- Linux: `libwebkit2gtk-4.1` and `libayatana-appindicator3` (the deb/rpm packages pull these in)

## Download

Get the artifacts of any build from [Actions](../../actions), or releases from [Releases](../../releases):

| Platform | Files |
| --- | --- |
| Windows | `*_x64-setup.exe` (NSIS installer), `*.msi`, `*_portable.exe` (single portable file) |
| macOS | `*_universal.dmg`, `*_macos_universal.app.zip` (Intel + Apple Silicon) |
| Linux | `*.AppImage`, `*.deb`, `*.rpm`, `*_linux_x64` (plain binary) |

> The macOS build is not notarized. The first time, right-click the app → Open, or run `xattr -cr /Applications/ssh2socks.app`.

## Remote forwards

By default sshd only lets remote forwards listen on the server's `127.0.0.1`. To make them reachable from outside, set this in the server's `/etc/ssh/sshd_config`:

```
GatewayPorts clientspecified   # or yes
```

Restart sshd afterwards, and make sure the server's firewall or cloud security group allows the port.

## Settings file

Compatible with the old Python version. Stored at:

- Linux: `~/.config/ssh2socks/tunnels.json`
- macOS: `~/Library/Application Support/ssh2socks/tunnels.json`
- Windows: `%APPDATA%\ssh2socks\tunnels.json`

Language and theme are saved in `settings.json` in the same folder.

## Building

Requires stable Rust, Node.js 20+, and the [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/).

```bash
npm ci
npx tauri dev      # run in development
npx tauri build    # package; output goes to src-tauri/target/release/bundle/
```

Tests and checks:

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## CI / releases

`.github/workflows/ci.yml`:

- Every push and PR: fmt, clippy and tests, then builds on Windows, macOS and Linux and uploads the artifacts
- Pushing a `v*` tag (e.g. `git tag v0.6.0 && git push origin v0.6.0`) also creates a GitHub Release with all installers attached

## License

Apache-2.0
