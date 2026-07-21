package core

import (
	"fmt"
	"sync"
	"syscall"
	"time"
)

type State string

const (
	StateStopped    State = "stopped"
	StateConnecting State = "connecting"
	StateConnected  State = "connected"
	StateError      State = "error"
)

const (
	defaultListen = "127.0.0.1:1080"
	defaultProbe  = "http://www.gstatic.com/generate_204"
	dialTimeout   = 15 * time.Second
	probeInterval = 30 * time.Second
	backoffBase   = 1 * time.Second
	backoffMax    = 30 * time.Second
)

// Config fully describes one tunnel to bring up.
type Config struct {
	// Chain source: either a config file + target alias, or a direct host.
	ConfigText string
	Target     string
	Host       string
	Port       string
	User       string

	PrivateKeyPEM []byte
	Passphrase    string
	DefaultUser   string

	ListenAddr    string
	ProbeURL      string
	AutoReconnect bool

	// Control, if set, is applied to the first hop's socket (VpnService.protect
	// on Android). Not marshalled from JSON; wired in by the binding layer.
	Control func(network, address string, c syscall.RawConn) error
}

// Events are optional callbacks; nil callbacks are ignored.
type Events struct {
	OnState func(state State, message string)
	OnLog   func(line string)
	OnProbe func(ok bool, latencyMS int64, message string)
}

type Engine struct {
	cfg Config
	ev  Events

	mu       sync.Mutex
	state    State
	dialer   *Dialer
	socks    *socksServer
	stopCh   chan struct{}
	running  bool
	attempts int
	wg       sync.WaitGroup
}

func NewEngine(cfg Config, ev Events) *Engine {
	if cfg.ListenAddr == "" {
		cfg.ListenAddr = defaultListen
	}
	if cfg.ProbeURL == "" {
		cfg.ProbeURL = defaultProbe
	}
	return &Engine{cfg: cfg, ev: ev, state: StateStopped}
}

func (e *Engine) Start() error {
	e.mu.Lock()
	if e.running {
		e.mu.Unlock()
		return fmt.Errorf("已在运行")
	}
	e.running = true
	e.stopCh = make(chan struct{})
	e.mu.Unlock()

	e.wg.Add(1)
	go e.loop()
	return nil
}

func (e *Engine) Stop() {
	e.mu.Lock()
	if !e.running {
		e.mu.Unlock()
		return
	}
	e.running = false
	close(e.stopCh)
	e.mu.Unlock()
	e.wg.Wait()
	e.setState(StateStopped, "")
}

// SocksAddr reports the active local SOCKS5 listen address (for tun2socks).
func (e *Engine) SocksAddr() string {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.socks != nil {
		return e.socks.addr()
	}
	return ""
}

func (e *Engine) loop() {
	defer e.wg.Done()
	for {
		if e.stopped() {
			return
		}
		e.setState(StateConnecting, "")
		if !e.connectOnce() {
			// connectOnce handles backoff sleep; loop again unless stopped
			if e.stopped() {
				return
			}
			if !e.cfg.AutoReconnect {
				e.setState(StateError, "连接失败")
				return
			}
			continue
		}
		// Connected. Block until drop or stop.
		reconnect := e.serve()
		e.teardown()
		if !reconnect {
			return
		}
	}
}

// connectOnce attempts to establish the SSH chain + SOCKS server once.
// Returns true on success. On failure it logs, and (if auto-reconnect) sleeps
// a backoff interval, returning false.
func (e *Engine) connectOnce() bool {
	hops, err := e.resolveChain()
	if err != nil {
		e.log("解析连接链失败: " + err.Error())
		return e.backoff()
	}
	auth := Auth{PrivateKeyPEM: e.cfg.PrivateKeyPEM, Passphrase: e.cfg.Passphrase, DefaultUser: e.cfg.DefaultUser, Control: e.cfg.Control}
	dialer, err := Dial(hops, auth, dialTimeout)
	if err != nil {
		e.log(err.Error())
		return e.backoff()
	}
	socks, err := startSocks(e.cfg.ListenAddr, dialer, e.log)
	if err != nil {
		dialer.Close()
		e.log("启动 SOCKS 失败: " + err.Error())
		return e.backoff()
	}
	e.mu.Lock()
	e.dialer = dialer
	e.socks = socks
	e.attempts = 0
	e.mu.Unlock()
	e.setState(StateConnected, socks.addr())
	e.log("SOCKS5 就绪于 " + socks.addr())
	return true
}

// serve blocks until the connection drops or Stop is called. Returns true if
// the caller should reconnect.
func (e *Engine) serve() bool {
	e.startProbe()
	dropped := make(chan struct{})
	go func() {
		_ = e.dialer.Wait()
		close(dropped)
	}()
	select {
	case <-e.stopCh:
		return false
	case <-dropped:
		e.log("SSH 连接已断开")
		return e.cfg.AutoReconnect
	}
}

func (e *Engine) startProbe() {
	go func() {
		e.probe()
		t := time.NewTicker(probeInterval)
		defer t.Stop()
		for {
			select {
			case <-e.stopCh:
				return
			case <-t.C:
				e.mu.Lock()
				d := e.dialer
				e.mu.Unlock()
				if d == nil {
					return
				}
				e.probe()
			}
		}
	}()
}

func (e *Engine) probe() {
	e.mu.Lock()
	d := e.dialer
	e.mu.Unlock()
	if d == nil || e.ev.OnProbe == nil {
		return
	}
	ok, ms, msg := probeOnce(d, e.cfg.ProbeURL)
	e.ev.OnProbe(ok, ms, msg)
}

func (e *Engine) teardown() {
	e.mu.Lock()
	socks := e.socks
	dialer := e.dialer
	e.socks = nil
	e.dialer = nil
	e.mu.Unlock()
	if socks != nil {
		socks.close()
	}
	if dialer != nil {
		dialer.Close()
	}
}

func (e *Engine) backoff() bool {
	if !e.cfg.AutoReconnect {
		return false
	}
	e.mu.Lock()
	n := e.attempts
	e.attempts++
	e.mu.Unlock()
	delay := backoffBase << n
	if delay > backoffMax {
		delay = backoffMax
	}
	e.log(fmt.Sprintf("%ds 后重连…", int(delay.Seconds())))
	select {
	case <-e.stopCh:
		return false
	case <-time.After(delay):
		return false // caller loops to retry
	}
}

func (e *Engine) resolveChain() ([]Hop, error) {
	if e.cfg.ConfigText != "" && e.cfg.Target != "" {
		cfg, err := parseConfig(e.cfg.ConfigText)
		if err != nil {
			return nil, err
		}
		return BuildChain(cfg, e.cfg.Target)
	}
	port := e.cfg.Port
	if port == "" {
		port = "22"
	}
	host := e.cfg.Host
	if host == "" {
		host = e.cfg.Target
	}
	if host == "" {
		return nil, fmt.Errorf("未指定目标主机")
	}
	return []Hop{{Alias: host, Host: host, Port: port, User: e.cfg.User}}, nil
}

func (e *Engine) stopped() bool {
	select {
	case <-e.stopCh:
		return true
	default:
		return false
	}
}

func (e *Engine) setState(s State, msg string) {
	e.mu.Lock()
	changed := e.state != s
	e.state = s
	e.mu.Unlock()
	if changed && e.ev.OnState != nil {
		e.ev.OnState(s, msg)
	}
}

func (e *Engine) log(line string) {
	if e.ev.OnLog != nil {
		e.ev.OnLog(line)
	}
}
