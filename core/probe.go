package core

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// probeOnce performs a small HTTP request to probeURL through the SSH chain and
// reports reachability + latency. Only http/https are supported.
func probeOnce(d *Dialer, probeURL string) (ok bool, latencyMS int64, message string) {
	if !strings.Contains(probeURL, "://") {
		probeURL = "http://" + probeURL
	}
	u, err := url.Parse(probeURL)
	if err != nil || u.Host == "" {
		return false, 0, "无效的探测地址"
	}

	transport := &http.Transport{
		DialContext: func(_ context.Context, _, addr string) (net.Conn, error) {
			// Ensure a port is present, then dial through the tunnel.
			if _, _, e := net.SplitHostPort(addr); e != nil {
				if u.Scheme == "https" {
					addr = net.JoinHostPort(addr, "443")
				} else {
					addr = net.JoinHostPort(addr, "80")
				}
			}
			return d.DialTCP(addr)
		},
		DisableKeepAlives: true,
	}
	client := &http.Client{Transport: transport, Timeout: 10 * time.Second}

	start := time.Now()
	req, err := http.NewRequest(http.MethodGet, probeURL, nil)
	if err != nil {
		return false, 0, err.Error()
	}
	req.Header.Set("User-Agent", "ssh2socks")
	resp, err := client.Do(req)
	ms := time.Since(start).Milliseconds()
	if err != nil {
		return false, ms, err.Error()
	}
	defer resp.Body.Close()
	if resp.StatusCode < 400 {
		return true, ms, "通"
	}
	return false, ms, fmt.Sprintf("HTTP %d", resp.StatusCode)
}
