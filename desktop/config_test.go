package main

import (
	"testing"

	"ssh2socks.local/core"
)

func TestFormToConfig_ConfigAlias(t *testing.T) {
	p := profile{Alias: "flabproxy", User: "loki", Listen: "127.0.0.1:1080", AutoReconnect: true}
	cfg, err := formToConfig(p, "Host flabproxy\n  HostName 10.0.0.1\n", "PEMDATA", "secret")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Target != "flabproxy" || cfg.ConfigText == "" {
		t.Fatalf("config-mode not set: target=%q configText=%q", cfg.Target, cfg.ConfigText)
	}
	if cfg.Host != "" {
		t.Fatalf("manual Host must stay empty in config mode, got %q", cfg.Host)
	}
	if cfg.Passphrase != "secret" || string(cfg.PrivateKeyPEM) != "PEMDATA" {
		t.Fatalf("secrets not carried: pass=%q key=%q", cfg.Passphrase, cfg.PrivateKeyPEM)
	}
	if cfg.DefaultUser != "loki" {
		t.Fatalf("DefaultUser = %q, want loki", cfg.DefaultUser)
	}
}

func TestFormToConfig_ManualHost(t *testing.T) {
	p := profile{Host: "example.com", Port: "2222", User: "root"}
	cfg, err := formToConfig(p, "", "PEM", "")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Host != "example.com" || cfg.Port != "2222" || cfg.User != "root" {
		t.Fatalf("manual fields wrong: %+v", cfg)
	}
	if cfg.ConfigText != "" || cfg.Target != "" {
		t.Fatalf("config-mode fields must be empty for manual host")
	}
	if cfg.ListenAddr != defaultListen {
		t.Fatalf("default listen not applied: %q", cfg.ListenAddr)
	}
}

// An alias is only honored when a config was actually loaded; alias without
// configText must fall through (here to manual host).
func TestFormToConfig_AliasWithoutConfigFallsToManual(t *testing.T) {
	p := profile{Alias: "flabproxy", Host: "example.com"}
	cfg, err := formToConfig(p, "", "PEM", "")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Target != "" || cfg.Host != "example.com" {
		t.Fatalf("expected manual host fallback, got target=%q host=%q", cfg.Target, cfg.Host)
	}
}

func TestFormToConfig_MissingKey(t *testing.T) {
	if _, err := formToConfig(profile{Host: "h"}, "", "  ", ""); err == nil {
		t.Fatal("expected error for missing private key")
	}
}

func TestFormToConfig_NoTarget(t *testing.T) {
	if _, err := formToConfig(profile{Listen: ":1"}, "", "PEM", ""); err == nil {
		t.Fatal("expected error when neither alias nor host is provided")
	}
}

func TestHostLabel(t *testing.T) {
	if got := hostLabel(core.HostInfo{Alias: "a"}); got != "a" {
		t.Fatalf("direct host label = %q", got)
	}
	got := hostLabel(core.HostInfo{Alias: "flabproxy", ProxyChain: "pc213 -> flabproxy"})
	if got != "flabproxy  (pc213 -> flabproxy)" {
		t.Fatalf("chained host label = %q", got)
	}
}
