package core

import (
	"strings"
	"testing"
)

const chainConfig = `
Host base
    HostName 10.0.0.1
    User root
    Port 22

Host mid
    HostName 10.0.0.2
    User admin
    ProxyCommand ssh -W %h:%p base

Host target
    HostName 10.0.0.3
    ProxyCommand ssh -W %h:%p mid

Host direct
    HostName 1.2.3.4

Host h1
    HostName 10.0.1.1
Host h2
    HostName 10.0.1.2

Host jumpjump
    HostName 10.0.0.9
    ProxyJump h1,h2
`

func mustChain(t *testing.T, cfgText, target string) []Hop {
	t.Helper()
	cfg, err := parseConfig(cfgText)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	hops, err := BuildChain(cfg, target)
	if err != nil {
		t.Fatalf("BuildChain(%s): %v", target, err)
	}
	return hops
}

func aliases(hops []Hop) []string {
	out := make([]string, len(hops))
	for i, h := range hops {
		out[i] = h.Alias
	}
	return out
}

func TestBuildChainProxyCommandOrder(t *testing.T) {
	hops := mustChain(t, chainConfig, "target")
	got := strings.Join(aliases(hops), ",")
	if got != "base,mid,target" {
		t.Fatalf("chain order = %q, want base,mid,target", got)
	}
	if hops[0].Host != "10.0.0.1" || hops[0].Port != "22" || hops[0].User != "root" {
		t.Fatalf("base hop resolved wrong: %+v", hops[0])
	}
	if hops[2].Host != "10.0.0.3" || hops[2].Port != "22" {
		t.Fatalf("target hop resolved wrong: %+v", hops[2])
	}
}

func TestBuildChainDirect(t *testing.T) {
	hops := mustChain(t, chainConfig, "direct")
	if len(hops) != 1 || hops[0].Host != "1.2.3.4" {
		t.Fatalf("direct chain wrong: %+v", hops)
	}
}

func TestBuildChainProxyJumpList(t *testing.T) {
	hops := mustChain(t, chainConfig, "jumpjump")
	got := strings.Join(aliases(hops), ",")
	if got != "h1,h2,jumpjump" {
		t.Fatalf("ProxyJump list order = %q, want h1,h2,jumpjump", got)
	}
}

func TestBuildChainCycleDetected(t *testing.T) {
	loop := `
Host a
    ProxyCommand ssh -W %h:%p b
Host b
    ProxyCommand ssh -W %h:%p a
`
	cfg, _ := parseConfig(loop)
	if _, err := BuildChain(cfg, "a"); err == nil {
		t.Fatal("expected cycle error, got nil")
	}
}

func TestListHostsAnnotatesChain(t *testing.T) {
	hosts, err := ListHosts(chainConfig)
	if err != nil {
		t.Fatalf("ListHosts: %v", err)
	}
	byAlias := map[string]HostInfo{}
	for _, h := range hosts {
		byAlias[h.Alias] = h
	}
	if _, ok := byAlias["target"]; !ok {
		t.Fatal("target missing from ListHosts")
	}
	if byAlias["target"].ProxyChain != "base -> mid -> target" {
		t.Fatalf("target chain = %q", byAlias["target"].ProxyChain)
	}
	if byAlias["direct"].ProxyChain != "" {
		t.Fatalf("direct should have no chain, got %q", byAlias["direct"].ProxyChain)
	}
}

func TestUnsupportedProxyCommand(t *testing.T) {
	cfg, _ := parseConfig("Host x\n    ProxyCommand corkscrew proxy 8080 %h %p\n")
	if _, err := BuildChain(cfg, "x"); err == nil {
		t.Fatal("expected error for unsupported ProxyCommand")
	}
}
