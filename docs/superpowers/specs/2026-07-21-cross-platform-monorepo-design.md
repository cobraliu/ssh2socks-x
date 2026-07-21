# ssh2socks-x — Cross-platform monorepo design

**Date:** 2026-07-21
**Status:** Approved (design), implementation in progress

## Goal

Consolidate the SSH→SOCKS proxy into a single cross-platform monorepo that shares
one Go engine (`core`) across:

- **Mobile** (Android now, iOS later) — Flutter/Kotlin UI + gomobile bridge + tun2socks VpnService.
- **Desktop** (Windows/macOS/Linux) — a **Fyne GUI client** (buttons, host list, status,
  logs) that reuses `core` directly. No tun, no VpnService — it just exposes a local
  SOCKS5 port that apps/browsers point at.

The prior Android app (repo `android/`, tag `v0.1.0`) and the Python/PySide desktop
(repo `ssh2socks/`) remain **untouched as backups**. All new work lives in the new
sibling repo `ssh2socks-x/`.

## Decisions (locked)

| Decision | Choice | Why |
|----------|--------|-----|
| Mobile UI | **Keep Flutter/Kotlin** | Android already shipped v0.1.0; reuse + add iOS later. |
| Desktop form | **GUI client (not CLI/TUI)** | Parity with the Flutter/PySide clients: buttons, host list, status. |
| Desktop UI toolkit | **Fyne** | Pure-Go, widget/button/form based, closest to PySide/Flutter dev model, cross-platform packaging. This is the "golang UI 体系". |
| Repo layout | **New sibling repo `ssh2socks-x/`** | Clean separation; old repos are the backup. |

### System-VPN reality (constrains "pure Go")

The OS VPN layer cannot be pure Go on mobile: Android `VpnService` and iOS
`NEPacketTunnelProvider` must be hosted in Kotlin/Swift, handing the `tun` fd to Go
via gomobile. On **desktop there is no tun at all** — `core` just runs the SSH chain
and a local SOCKS5 server, so the desktop client is genuinely all-Go.

## Repository layout

```
ssh2socks-x/
├── go.work                     # dev workspace: ./core ./desktop (mobile excluded — own toolchain)
├── core/       MIGRATED  — module ssh2socks.local/core; SSH chain, SOCKS5, DNS-over-TCP,
│                            reconnect, HTTP probe, OpenSSH config + ProxyJump/ProxyCommand
├── mobile/     MIGRATED  — module ssh2socks.local/mobile; gomobile bridge (.aar now,
│                            .xcframework later); keeps `replace ssh2socks.local/core => ../core`
├── desktop/    NEW       — module ssh2socks.local/desktop; Fyne GUI; reuses core directly
├── app/        MIGRATED  — Flutter + Kotlin Android UI
├── scripts/    build_aar.sh, ci_assemble.sh (migrated) + build_gui.sh (new)
├── .github/workflows/    android-apk.yml (migrated) + desktop-gui.yml (new)
├── docs/superpowers/specs/
└── README.md               NEW — cross-platform overview
```

**go.work** lists only `./core` and `./desktop` (both pure-Go, offline-buildable). `mobile`
is intentionally excluded — it pulls tun2socks and only builds inside the gomobile/NDK
toolchain, where `build_aar.sh` sets `GOWORK=off` and relies on its `replace` directive.

## `core` — the shared engine (unchanged)

Consumed directly by the desktop, and via the gomobile bridge on mobile.

- `core.NewEngine(cfg Config, ev Events) *Engine`; `Start() error`; `Stop()`; `SocksAddr() string`.
- `Config{ ConfigText, Target, Host, Port, User, PrivateKeyPEM, Passphrase, DefaultUser,
  ListenAddr, ProbeURL, AutoReconnect, Control }`. On desktop `Control = nil` (no tun).
- `Events{ OnState(State,msg), OnLog(line), OnProbe(ok,latencyMS,msg) }` — fire on core
  goroutines.
- `core.ListHosts(configText) ([]HostInfo, error)` where
  `HostInfo{Alias, HostName, User, Port, ProxyChain}` — powers the desktop host dropdown.

No changes to `core` are required for the desktop; if host-listing or key helpers need a
tiny exported shim, add it to `core` (never duplicate parsing in the desktop).

## Desktop — Fyne GUI client

Module `ssh2socks.local/desktop`, `replace ssh2socks.local/core => ../core`. No gomobile,
no tun2socks.

### UI

A single main window:

- **Source:** "Import SSH config…" file picker → populate a **host dropdown** from
  `core.ListHosts` (each item shows alias + `ProxyChain`, e.g. `pc213 -> flabproxy`).
  Alternatively a manual **host / port / user** row.
- **Auth:** identity **key-file picker** (read PEM into `Config.PrivateKeyPEM`); **passphrase**
  entry (masked).
- **Options:** listen addr (default `127.0.0.1:1080`), probe URL, **auto-reconnect** checkbox.
- **Controls:** **Start/Stop** button; colored **status indicator** (stopped / connecting /
  connected / error); **SOCKS address** label with a **Copy** button; scrolling **log pane**.

### Wiring

- Build a `core.Config` from the form (pure function `formToConfig(form) (core.Config, error)`
  — unit-tested without a GUI).
- `core.Events` callbacks arrive on core goroutines → marshalled onto Fyne's UI thread via
  `fyne.Do(...)` → update status color, SOCKS label, probe latency, and append to the log pane.
- Start disables inputs and flips the button to Stop; Stop calls `engine.Stop()` and re-enables.

### Persistence

Last profile (config path, selected alias/manual fields, listen, probe, auto-reconnect) saved
via Fyne `Preferences`. **Passphrase is NOT persisted** in v1 (entered per run). OS-keyring
storage is a later follow-up (YAGNI).

### Packaging

`scripts/build_gui.sh` uses the `fyne` CLI (`fyne package`) to produce Windows/macOS/Linux
bundles. Linux build needs the usual GL/X11 dev headers (documented in README).

## Migration

- Copy **git-tracked files only** from `android/` (via `git archive HEAD`) — no `.gradle`,
  `.dart_tool`, `mobile.aar`, or build outputs.
- Module paths unchanged (`ssh2socks.local/core`, `…/mobile`) to avoid churn.
- **No behavior change** to the shipped Android path; "refactor" = repackaging + clean sharing
  of `core`.

## Testing

- All existing `core` tests migrate verbatim: `go test ./core/...` — config/chain unit tests
  plus the **isolated throwaway-sshd 2-hop E2E that spins up its own sshd in a temp dir and
  never touches the real `~/.ssh`**.
- Desktop: unit-test the pure `formToConfig` mapping and the `core.ListHosts` → dropdown
  transform. GUI event wiring kept thin (no headless-GUI test in v1).

## CI

- `android-apk.yml` migrated as-is.
- `desktop-gui.yml` (new): build + `fyne package` for the three desktop OSes, upload artifacts,
  attach to tag releases.
- Workflow files are committed but **not pushed** from here — the current token lacks the
  `workflow` scope, and the new remote/signing will be set up by the maintainer.

## Out of scope (v1 / follow-ups)

- iOS (`NEPacketTunnelProvider`) — folds into `mobile/` later.
- OS-keyring passphrase storage on desktop.
- `known_hosts` / host-key verification (still `InsecureIgnoreHostKey`, inherited from core).
- Arbitrary UDP (only DNS-over-TCP is proxied; non-DNS UDP dropped — inherited from core).
