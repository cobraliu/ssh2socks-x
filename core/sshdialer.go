package core

import (
	"fmt"
	"net"
	"syscall"
	"time"

	"golang.org/x/crypto/ssh"
)

// Auth carries the (single) private key applied to every hop in the chain,
// which matches typical personal setups. Passphrase may be empty.
type Auth struct {
	PrivateKeyPEM []byte
	Passphrase    string
	// DefaultUser is used for any hop whose config has no User.
	DefaultUser string
	// Control, if set, is applied to the raw socket of the first hop before it
	// connects. On Android this calls VpnService.protect(fd) so the underlying
	// SSH connection escapes the tun and does not loop back through tun2socks.
	Control func(network, address string, c syscall.RawConn) error
}

func (a Auth) signer() (ssh.Signer, error) {
	if len(a.PrivateKeyPEM) == 0 {
		return nil, fmt.Errorf("未提供私钥")
	}
	if a.Passphrase != "" {
		return ssh.ParsePrivateKeyWithPassphrase(a.PrivateKeyPEM, []byte(a.Passphrase))
	}
	return ssh.ParsePrivateKey(a.PrivateKeyPEM)
}

func (h Hop) user(auth Auth) (string, error) {
	if h.User != "" {
		return h.User, nil
	}
	if auth.DefaultUser != "" {
		return auth.DefaultUser, nil
	}
	return "", fmt.Errorf("跳板 %s 未指定用户名", h.Alias)
}

// Dialer holds an established (possibly chained) SSH connection to the target.
type Dialer struct {
	clients []*ssh.Client // clients[len-1] is the target
}

// Dial builds the SSH connection following the hop chain. hops[0] is dialed
// directly; each subsequent hop is reached through the previous client.
func Dial(hops []Hop, auth Auth, timeout time.Duration) (*Dialer, error) {
	if len(hops) == 0 {
		return nil, fmt.Errorf("空的连接链")
	}
	signer, err := auth.signer()
	if err != nil {
		return nil, fmt.Errorf("加载私钥失败: %w", err)
	}

	d := &Dialer{}
	for i, hop := range hops {
		user, err := hop.user(auth)
		if err != nil {
			d.Close()
			return nil, err
		}
		cfg := &ssh.ClientConfig{
			User:            user,
			Auth:            []ssh.AuthMethod{ssh.PublicKeys(signer)},
			HostKeyCallback: ssh.InsecureIgnoreHostKey(), // v1: TOFU/known_hosts is a follow-up
			Timeout:         timeout,
		}
		if i == 0 {
			nd := net.Dialer{Timeout: timeout, Control: auth.Control}
			raw, err := nd.Dial("tcp", hop.Addr())
			if err != nil {
				d.Close()
				return nil, fmt.Errorf("连接 %s 失败: %w", hop.Alias, err)
			}
			conn, chans, reqs, err := ssh.NewClientConn(raw, hop.Addr(), cfg)
			if err != nil {
				raw.Close()
				d.Close()
				return nil, fmt.Errorf("与 %s 握手失败: %w", hop.Alias, err)
			}
			d.clients = append(d.clients, ssh.NewClient(conn, chans, reqs))
			continue
		}
		// Tunnel a TCP connection to this hop through the previous client.
		prev := d.clients[i-1]
		raw, err := prev.Dial("tcp", hop.Addr())
		if err != nil {
			d.Close()
			return nil, fmt.Errorf("经 %s 连接 %s 失败: %w", hops[i-1].Alias, hop.Alias, err)
		}
		conn, chans, reqs, err := ssh.NewClientConn(raw, hop.Addr(), cfg)
		if err != nil {
			raw.Close()
			d.Close()
			return nil, fmt.Errorf("与 %s 握手失败: %w", hop.Alias, err)
		}
		d.clients = append(d.clients, ssh.NewClient(conn, chans, reqs))
	}
	return d, nil
}

// DialTCP opens a connection to addr from the target host (used by the SOCKS server).
func (d *Dialer) DialTCP(addr string) (net.Conn, error) {
	if len(d.clients) == 0 {
		return nil, fmt.Errorf("连接未建立")
	}
	return d.clients[len(d.clients)-1].Dial("tcp", addr)
}

// Wait blocks until the target connection drops.
func (d *Dialer) Wait() error {
	if len(d.clients) == 0 {
		return fmt.Errorf("连接未建立")
	}
	return d.clients[len(d.clients)-1].Wait()
}

// Close tears down every client in the chain, innermost first.
func (d *Dialer) Close() {
	for i := len(d.clients) - 1; i >= 0; i-- {
		_ = d.clients[i].Close()
	}
	d.clients = nil
}
