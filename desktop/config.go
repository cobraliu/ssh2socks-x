package main

import (
	"fmt"
	"strings"

	"fyne.io/fyne/v2"
	"ssh2socks.local/core"
)

const defaultListen = "127.0.0.1:1080"

// profile is the user-editable form state, persisted between runs (never secrets).
type profile struct {
	ConfigPath    string // path to an OpenSSH config file (optional)
	Alias         string // selected host alias from the config
	Host          string // manual host (used only when no config alias is chosen)
	Port          string
	User          string
	IdentityPath  string
	Listen        string
	ProbeURL      string
	AutoReconnect bool
}

// formToConfig converts the form state plus the live secret inputs into a
// core.Config. It is pure (no I/O, no globals) so it can be unit-tested without
// a GUI: configText is the already-read OpenSSH config, keyPEM the already-read
// private key, passphrase the never-persisted secret.
func formToConfig(p profile, configText, keyPEM, passphrase string) (core.Config, error) {
	cfg := core.Config{
		ListenAddr:    strings.TrimSpace(p.Listen),
		ProbeURL:      strings.TrimSpace(p.ProbeURL),
		AutoReconnect: p.AutoReconnect,
		Passphrase:    passphrase,
		DefaultUser:   strings.TrimSpace(p.User),
	}
	if cfg.ListenAddr == "" {
		cfg.ListenAddr = defaultListen
	}
	if strings.TrimSpace(keyPEM) == "" {
		return core.Config{}, fmt.Errorf("请选择私钥文件")
	}
	cfg.PrivateKeyPEM = []byte(keyPEM)

	alias := strings.TrimSpace(p.Alias)
	host := strings.TrimSpace(p.Host)
	switch {
	case configText != "" && alias != "":
		cfg.ConfigText = configText
		cfg.Target = alias
	case host != "":
		cfg.Host = host
		cfg.Port = strings.TrimSpace(p.Port)
		cfg.User = strings.TrimSpace(p.User)
	default:
		return core.Config{}, fmt.Errorf("请选择一个主机（导入配置并选择别名，或填写主机名）")
	}
	return cfg, nil
}

// hostLabel is the dropdown text for a config host: alias plus its proxy chain
// (e.g. "flabproxy  (pc213 -> flabproxy)") so jump-host targets are obvious.
func hostLabel(h core.HostInfo) string {
	if h.ProxyChain != "" {
		return h.Alias + "  (" + h.ProxyChain + ")"
	}
	return h.Alias
}

// --- Preferences persistence (non-secret fields only) ---

const (
	prefConfigPath   = "configPath"
	prefAlias        = "alias"
	prefHost         = "host"
	prefPort         = "port"
	prefUser         = "user"
	prefIdentityPath = "identityPath"
	prefListen       = "listen"
	prefProbeURL     = "probeURL"
	prefAutoReconn   = "autoReconnect"
)

func loadProfile(p fyne.Preferences) profile {
	return profile{
		ConfigPath:    p.String(prefConfigPath),
		Alias:         p.String(prefAlias),
		Host:          p.String(prefHost),
		Port:          p.String(prefPort),
		User:          p.String(prefUser),
		IdentityPath:  p.String(prefIdentityPath),
		Listen:        p.StringWithFallback(prefListen, defaultListen),
		ProbeURL:      p.String(prefProbeURL),
		AutoReconnect: p.BoolWithFallback(prefAutoReconn, true),
	}
}

func saveProfile(p fyne.Preferences, v profile) {
	p.SetString(prefConfigPath, v.ConfigPath)
	p.SetString(prefAlias, v.Alias)
	p.SetString(prefHost, v.Host)
	p.SetString(prefPort, v.Port)
	p.SetString(prefUser, v.User)
	p.SetString(prefIdentityPath, v.IdentityPath)
	p.SetString(prefListen, v.Listen)
	p.SetString(prefProbeURL, v.ProbeURL)
	p.SetBool(prefAutoReconn, v.AutoReconnect)
}
