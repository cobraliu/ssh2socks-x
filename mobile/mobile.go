// Package mobile is the gomobile bind surface consumed by the Android app
// (Kotlin/Flutter). It wraps the pure `core` engine and bridges the tun device
// provided by VpnService to the in-process SOCKS5 proxy via tun2socks.
//
// gomobile type rules honoured here: exported functions/methods use only
// primitives, string, []byte, error, and bound struct pointers; callbacks flow
// through the Callback interface implemented on the Kotlin side.
package mobile

import (
	"encoding/json"
	"fmt"
	"sync"
	"syscall"

	"github.com/xjasonlyu/tun2socks/v2/engine"
	"ssh2socks.local/core"
)

// Callback is implemented on the Kotlin side to receive engine events.
// int64 latency is narrowed to int for a friendlier Java signature.
type Callback interface {
	OnState(state string, message string)
	OnLog(line string)
	OnProbe(ok bool, latencyMS int, message string)
}

// Protector is implemented on the Kotlin side; it must call
// VpnService.protect(fd) so the underlying SSH socket bypasses the tunnel.
type Protector interface {
	Protect(fd int) bool
}

// controlFrom builds a net.Dialer Control hook that protects the raw socket fd.
// Returns nil when no protector is supplied (e.g. non-VPN/desktop use).
func controlFrom(p Protector) func(network, address string, c syscall.RawConn) error {
	if p == nil {
		return nil
	}
	return func(_, _ string, c syscall.RawConn) error {
		return c.Control(func(fd uintptr) {
			p.Protect(int(fd))
		})
	}
}

// jsonConfig mirrors the UI-relevant subset of core.Config. The private key is
// passed as a PEM string (from Android Keystore / secure storage).
type jsonConfig struct {
	ConfigText    string `json:"configText"`
	Target        string `json:"target"`
	Host          string `json:"host"`
	Port          string `json:"port"`
	User          string `json:"user"`
	PrivateKeyPEM string `json:"privateKeyPem"`
	Passphrase    string `json:"passphrase"`
	DefaultUser   string `json:"defaultUser"`
	ListenAddr    string `json:"listenAddr"`
	ProbeURL      string `json:"probeUrl"`
	AutoReconnect bool   `json:"autoReconnect"`
}

// Tunnel is one live SSH→SOCKS engine plus its tun2socks binding.
type Tunnel struct {
	mu        sync.Mutex
	eng       *core.Engine
	tunActive bool
}

// NewTunnel builds an engine from a JSON config (see jsonConfig), a callback,
// and a socket protector (may be nil for non-VPN use).
func NewTunnel(cfgJSON string, cb Callback, protector Protector) (*Tunnel, error) {
	var jc jsonConfig
	if err := json.Unmarshal([]byte(cfgJSON), &jc); err != nil {
		return nil, fmt.Errorf("解析配置失败: %w", err)
	}
	cfg := core.Config{
		ConfigText:    jc.ConfigText,
		Target:        jc.Target,
		Host:          jc.Host,
		Port:          jc.Port,
		User:          jc.User,
		PrivateKeyPEM: []byte(jc.PrivateKeyPEM),
		Passphrase:    jc.Passphrase,
		DefaultUser:   jc.DefaultUser,
		ListenAddr:    jc.ListenAddr,
		ProbeURL:      jc.ProbeURL,
		AutoReconnect: jc.AutoReconnect,
		Control:       controlFrom(protector),
	}
	ev := core.Events{
		OnState: func(s core.State, m string) {
			if cb != nil {
				cb.OnState(string(s), m)
			}
		},
		OnLog: func(l string) {
			if cb != nil {
				cb.OnLog(l)
			}
		},
		OnProbe: func(ok bool, ms int64, msg string) {
			if cb != nil {
				cb.OnProbe(ok, int(ms), msg)
			}
		},
	}
	return &Tunnel{eng: core.NewEngine(cfg, ev)}, nil
}

// Start brings up the SSH chain and local SOCKS5 listener.
func (t *Tunnel) Start() error { return t.eng.Start() }

// Stop tears down tun2socks (if running) and the SSH chain.
func (t *Tunnel) Stop() {
	t.StopTun()
	t.eng.Stop()
}

// SocksAddr reports the active local SOCKS5 address, or "" if not ready.
func (t *Tunnel) SocksAddr() string { return t.eng.SocksAddr() }

// StartTun connects the VpnService tun file descriptor to the in-process SOCKS5
// proxy. fd must come from ParcelFileDescriptor.detachFd(). Call only after the
// engine reports the "connected" state.
func (t *Tunnel) StartTun(fd int, mtu int) error {
	addr := t.eng.SocksAddr()
	if addr == "" {
		return fmt.Errorf("SOCKS 尚未就绪")
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.tunActive {
		return nil
	}
	key := &engine.Key{
		MTU:      mtu,
		Device:   fmt.Sprintf("fd://%d", fd),
		Proxy:    "socks5://" + addr,
		LogLevel: "warning",
	}
	engine.Insert(key)
	engine.Start()
	t.tunActive = true
	return nil
}

// StopTun stops the tun2socks engine. Safe to call when inactive.
func (t *Tunnel) StopTun() {
	t.mu.Lock()
	active := t.tunActive
	t.tunActive = false
	t.mu.Unlock()
	if active {
		engine.Stop()
	}
}

// ListHosts returns the config file's selectable hosts as a JSON array string:
// [{"alias","hostName","user","port","proxyChain"}, ...]
func ListHosts(configText string) (string, error) {
	hosts, err := core.ListHosts(configText)
	if err != nil {
		return "", err
	}
	b, err := json.Marshal(hosts)
	if err != nil {
		return "", err
	}
	return string(b), nil
}
