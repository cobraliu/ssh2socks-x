# ssh2socks-x

Cross-platform SSH→SOCKS proxy. One shared Go engine (`core`) turns an SSH
connection — including jump-host chains — into a local SOCKS5 proxy, wrapped in
native clients on each platform.

| Platform | UI | How it proxies |
|----------|----|----------------|
| **Desktop** (Windows/macOS/Linux) | **Fyne GUI** (`desktop/`) | Opens a local SOCKS5 port; point your browser/apps at it. No VPN/tun. |
| **Android** | **Flutter + Kotlin** (`app/`) | `VpnService` + tun2socks routes device/per-app traffic through the SOCKS port. |
| **iOS** | *(planned)* | `NEPacketTunnelProvider` + tun2socks. Folds into `mobile/`. |

## Layout

```
core/       Go module ssh2socks.local/core — the shared engine:
              SSH chain (hop → hop → target, private-key auth), local SOCKS5,
              DNS-over-TCP, auto-reconnect, HTTP connectivity probe,
              OpenSSH config + ProxyJump/ProxyCommand parsing.
desktop/    Go module ssh2socks.local/desktop — Fyne GUI; reuses core directly.
mobile/     Go module ssh2socks.local/mobile — gomobile bridge (.aar today,
              .xcframework later); wraps core + tun2socks for Kotlin/Swift.
app/        Flutter + Kotlin Android client.
scripts/    build_aar.sh, ci_assemble.sh (Android) · build_gui.sh (desktop).
go.work     Dev workspace tying ./core + ./desktop (see note below).
```

`go.work` lists only `core` and `desktop` — both are pure Go and build/test
offline. `mobile` is deliberately excluded: it pulls tun2socks and only builds
inside the gomobile/NDK toolchain, where `build_aar.sh` sets `GOWORK=off` and
relies on its own `replace` directive.

> The environment may export `GOFLAGS=-mod=mod`, which workspace mode rejects.
> The scripts clear it; if you run `go` by hand, use `GOFLAGS=` (and `GOWORK=off`
> for `go mod tidy`).

## The shared engine (`core`)

- **Auth:** private key only (with passphrase); no password auth.
- **Chains:** `ProxyJump a,b,c` and `ProxyCommand ssh -W %h:%p <jump>` are resolved
  natively into an ordered hop chain and dialed in-process — no external `ssh`
  binary. Other `ProxyCommand` forms are rejected with a clear error.
- **UDP / DNS:** SSH `direct-tcpip` carries only TCP, so the SOCKS5 server answers
  `UDP ASSOCIATE` in a restricted way — DNS (UDP :53) is translated to
  **DNS-over-TCP** (RFC 7766) through the chain; all other UDP (e.g. QUIC on
  UDP/443) is dropped so clients fall back to TCP.
- **Security note:** host keys use `InsecureIgnoreHostKey` (TOFU/`known_hosts` is a
  planned follow-up). Fine for trusted jump hosts you control; audit before wider use.

## Desktop (Fyne)

A GUI client with a host list, Start/Stop, status indicator, SOCKS address, and a
log pane. It reuses `core.Engine` directly and never touches a tun device — it
simply exposes `127.0.0.1:1080` (configurable) as SOCKS5.

```bash
# Prereqs: Go >= 1.26, a C compiler, and on Linux the GL/X11 dev headers:
#   Debian/Ubuntu: sudo apt-get install libgl1-mesa-dev xorg-dev
scripts/build_gui.sh          # → desktop/ssh2socks-desktop (host binary)
./desktop/ssh2socks-desktop
```

Usage: **Import…** an OpenSSH config and pick a host (jump chains shown inline),
or fill host/port/user manually; choose a **private key** and enter its passphrase
(not persisted); set the listen address; **Start**. Point your browser/apps at the
shown SOCKS5 address.

## Android

See the build flow in `scripts/build_aar.sh` + `scripts/ci_assemble.sh` and the
`.github/workflows/android-apk.yml` pipeline. Unchanged from the shipped `v0.1.0`
line; migrated here verbatim.

## Verify the engine locally (no GUI, no device)

```bash
cd core
GOFLAGS= go test ./...     # config/chain unit tests + a real 2-hop sshd E2E
```

The E2E test spins up a throwaway `sshd` in a temp dir with its own host/client
keys — it never touches your real `~/.ssh`.

## CI

- `desktop-gui.yml` — builds the Fyne client on Linux/macOS/Windows, uploads a
  binary per OS, and attaches them to `v*` tag releases.
- `android-apk.yml` — builds the release-signed APK (migrated).

Committing workflow files requires a token with the `workflow` scope; if a push
is rejected for that, add them through the GitHub web UI.
