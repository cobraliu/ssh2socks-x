package core

import (
	"fmt"
	"regexp"
	"strings"

	"github.com/kevinburke/ssh_config"
)

// Hop is one SSH server in a (possibly multi-hop) connection chain.
type Hop struct {
	Alias    string
	Host     string // resolved HostName (falls back to Alias)
	Port     string // defaults to "22"
	User     string // may be empty if the config does not specify one
	Identity string // IdentityFile from config, informational only
}

func (h Hop) Addr() string { return h.Host + ":" + h.Port }

// HostInfo is a config entry surfaced to the UI for the import picker.
type HostInfo struct {
	Alias      string `json:"alias"`
	HostName   string `json:"hostName"`
	User       string `json:"user"`
	Port       string `json:"port"`
	ProxyChain string `json:"proxyChain"` // e.g. "idc101 -> target", empty if direct
}

// matches `ssh -W %h:%p <jump>` style ProxyCommand and captures the jump host.
var proxyWWithHost = regexp.MustCompile(`\bssh\b.*-W\s+\S+\s+([^\s]+)`)

func parseConfig(text string) (*ssh_config.Config, error) {
	return ssh_config.Decode(strings.NewReader(text))
}

// directJumps returns the immediate jump aliases required before reaching
// alias, in connect order (outermost first). It understands both
// `ProxyJump a,b` (comma list) and `ProxyCommand ssh -W %h:%p j`.
func directJumps(cfg *ssh_config.Config, alias string) ([]string, error) {
	pj, _ := cfg.Get(alias, "ProxyJump")
	pj = strings.TrimSpace(pj)
	if pj != "" && !strings.EqualFold(pj, "none") {
		var jumps []string
		for _, part := range strings.Split(pj, ",") {
			part = strings.TrimSpace(part)
			if part == "" {
				continue
			}
			// strip optional user@ and :port, keep the host token
			if at := strings.LastIndex(part, "@"); at >= 0 {
				part = part[at+1:]
			}
			if colon := strings.Index(part, ":"); colon >= 0 {
				part = part[:colon]
			}
			jumps = append(jumps, part)
		}
		return jumps, nil
	}

	pc, _ := cfg.Get(alias, "ProxyCommand")
	pc = strings.TrimSpace(pc)
	if pc != "" && !strings.EqualFold(pc, "none") {
		m := proxyWWithHost.FindStringSubmatch(pc)
		if m != nil {
			return []string{m[1]}, nil
		}
		return nil, fmt.Errorf("不支持的 ProxyCommand（仅支持 `ssh -W %%h:%%p 跳板`）: %s", pc)
	}
	return nil, nil
}

func hopFor(cfg *ssh_config.Config, alias string) Hop {
	get := func(key string) string {
		v, _ := cfg.Get(alias, key)
		return strings.TrimSpace(v)
	}
	host := get("HostName")
	if host == "" {
		host = alias
	}
	port := get("Port")
	if port == "" {
		port = "22"
	}
	return Hop{
		Alias:    alias,
		Host:     host,
		Port:     port,
		User:     get("User"),
		Identity: get("IdentityFile"),
	}
}

// BuildChain resolves the full ordered hop list to reach target: the first
// element is dialed directly, each subsequent hop is reached through the
// previous one, and the last element is the target itself.
func BuildChain(cfg *ssh_config.Config, target string) ([]Hop, error) {
	seen := map[string]bool{}
	var order []string
	var walk func(alias string) error
	walk = func(alias string) error {
		if seen[alias] {
			return fmt.Errorf("检测到跳板环路: %s", alias)
		}
		seen[alias] = true
		jumps, err := directJumps(cfg, alias)
		if err != nil {
			return err
		}
		for _, j := range jumps {
			if err := walk(j); err != nil {
				return err
			}
		}
		order = append(order, alias)
		return nil
	}
	if err := walk(target); err != nil {
		return nil, err
	}
	hops := make([]Hop, 0, len(order))
	for _, alias := range order {
		hops = append(hops, hopFor(cfg, alias))
	}
	return hops, nil
}

// ListHosts extracts concrete (non-wildcard) host aliases from config text for
// the UI import picker, annotating each with its resolved proxy chain.
func ListHosts(configText string) ([]HostInfo, error) {
	cfg, err := parseConfig(configText)
	if err != nil {
		return nil, err
	}
	var out []HostInfo
	seen := map[string]bool{}
	for _, h := range cfg.Hosts {
		for _, pat := range h.Patterns {
			alias := pat.String()
			if alias == "" || strings.ContainsAny(alias, "*?!") || seen[alias] {
				continue
			}
			seen[alias] = true
			hop := hopFor(cfg, alias)
			info := HostInfo{
				Alias:    alias,
				HostName: hop.Host,
				User:     hop.User,
				Port:     hop.Port,
			}
			if chain, cerr := BuildChain(cfg, alias); cerr == nil && len(chain) > 1 {
				names := make([]string, len(chain))
				for i, hp := range chain {
					names[i] = hp.Alias
				}
				info.ProxyChain = strings.Join(names, " -> ")
			}
			out = append(out, info)
		}
	}
	return out, nil
}
