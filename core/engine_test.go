package core

import (
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

// sshdFixture is an isolated OpenSSH server used only for tests. It touches
// none of the user's real ~/.ssh state.
type sshdFixture struct {
	port   int
	user   string
	keyPEM []byte
	cmd    *exec.Cmd
}

func findSSHD(t *testing.T) string {
	for _, c := range []string{"sshd", "/usr/sbin/sshd", "/usr/local/sbin/sshd"} {
		if p, err := exec.LookPath(c); err == nil {
			return p
		}
		if _, err := os.Stat(c); err == nil {
			return c
		}
	}
	t.Skip("sshd not found; skipping integration test")
	return ""
}

func freePort(t *testing.T) int {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}

func keygen(t *testing.T, path string) {
	if _, err := exec.LookPath("ssh-keygen"); err != nil {
		t.Skip("ssh-keygen not found; skipping integration test")
	}
	cmd := exec.Command("ssh-keygen", "-t", "ed25519", "-f", path, "-N", "", "-q")
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("ssh-keygen: %v\n%s", err, out)
	}
}

func startSSHD(t *testing.T) *sshdFixture {
	sshd := findSSHD(t)
	dir := t.TempDir()
	hostKey := filepath.Join(dir, "host")
	clientKey := filepath.Join(dir, "client")
	keygen(t, hostKey)
	keygen(t, clientKey)

	pub, err := os.ReadFile(clientKey + ".pub")
	if err != nil {
		t.Fatal(err)
	}
	authKeys := filepath.Join(dir, "authorized_keys")
	if err := os.WriteFile(authKeys, pub, 0600); err != nil {
		t.Fatal(err)
	}
	keyPEM, err := os.ReadFile(clientKey)
	if err != nil {
		t.Fatal(err)
	}

	me, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	port := freePort(t)
	conf := fmt.Sprintf(`Port %d
ListenAddress 127.0.0.1
HostKey %s
AuthorizedKeysFile %s
PasswordAuthentication no
PubkeyAuthentication yes
UsePAM no
StrictModes no
PidFile %s
`, port, hostKey, authKeys, filepath.Join(dir, "sshd.pid"))
	confPath := filepath.Join(dir, "sshd_config")
	if err := os.WriteFile(confPath, []byte(conf), 0600); err != nil {
		t.Fatal(err)
	}

	cmd := exec.Command(sshd, "-D", "-e", "-f", confPath)
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start sshd: %v", err)
	}

	// wait for port
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if c, err := net.Dial("tcp", fmt.Sprintf("127.0.0.1:%d", port)); err == nil {
			c.Close()
			return &sshdFixture{port: port, user: me.Username, keyPEM: keyPEM, cmd: cmd}
		}
		time.Sleep(100 * time.Millisecond)
	}
	cmd.Process.Kill()
	t.Skip("sshd did not come up (likely sandbox restriction); skipping integration test")
	return nil
}

func (f *sshdFixture) stop() {
	if f != nil && f.cmd != nil && f.cmd.Process != nil {
		_ = f.cmd.Process.Kill()
		_ = f.cmd.Wait()
	}
}

// socksGet performs an HTTP GET to targetURL through a SOCKS5 proxy at proxyAddr.
func socksGet(proxyAddr, host string, port int, path string) (string, error) {
	c, err := net.DialTimeout("tcp", proxyAddr, 3*time.Second)
	if err != nil {
		return "", err
	}
	defer c.Close()
	c.SetDeadline(time.Now().Add(5 * time.Second))
	// greeting
	if _, err := c.Write([]byte{0x05, 0x01, 0x00}); err != nil {
		return "", err
	}
	rep := make([]byte, 2)
	if _, err := io.ReadFull(c, rep); err != nil {
		return "", err
	}
	if rep[1] != 0x00 {
		return "", fmt.Errorf("socks auth rejected")
	}
	// CONNECT by domain
	hb := []byte(host)
	req := []byte{0x05, 0x01, 0x00, 0x03, byte(len(hb))}
	req = append(req, hb...)
	pb := make([]byte, 2)
	binary.BigEndian.PutUint16(pb, uint16(port))
	req = append(req, pb...)
	if _, err := c.Write(req); err != nil {
		return "", err
	}
	head := make([]byte, 4)
	if _, err := io.ReadFull(c, head); err != nil {
		return "", err
	}
	if head[1] != 0x00 {
		return "", fmt.Errorf("socks connect failed code %d", head[1])
	}
	// consume bound addr
	switch head[3] {
	case 0x01:
		io.ReadFull(c, make([]byte, 4+2))
	case 0x04:
		io.ReadFull(c, make([]byte, 16+2))
	case 0x03:
		l := make([]byte, 1)
		io.ReadFull(c, l)
		io.ReadFull(c, make([]byte, int(l[0])+2))
	}
	fmt.Fprintf(c, "GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", path, host)
	data, _ := io.ReadAll(c)
	return string(data), nil
}

func waitState(ch <-chan State, want State, d time.Duration) bool {
	deadline := time.After(d)
	for {
		select {
		case s := <-ch:
			if s == want {
				return true
			}
		case <-deadline:
			return false
		}
	}
}

// TestEndToEndThroughJump exercises config parsing, a 2-hop ProxyCommand chain,
// SSH dialing, the SOCKS5 server and the probe — all against a throwaway sshd.
func TestEndToEndThroughJump(t *testing.T) {
	fx := startSSHD(t)
	defer fx.stop()

	// local HTTP target reached *through* the tunnel
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(204)
	}))
	defer srv.Close()
	targetHost, targetPortStr, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))
	targetPort, _ := strconv.Atoi(targetPortStr)

	cfgText := fmt.Sprintf(`
Host jumpbox
    HostName 127.0.0.1
    Port %d
    User %s

Host target
    HostName 127.0.0.1
    Port %d
    User %s
    ProxyCommand ssh -W %%h:%%p jumpbox
`, fx.port, fx.user, fx.port, fx.user)

	socksPort := freePort(t)
	states := make(chan State, 16)
	probes := make(chan bool, 8)
	eng := NewEngine(Config{
		ConfigText:    cfgText,
		Target:        "target",
		PrivateKeyPEM: fx.keyPEM,
		DefaultUser:   fx.user,
		ListenAddr:    fmt.Sprintf("127.0.0.1:%d", socksPort),
		ProbeURL:      srv.URL,
		AutoReconnect: false,
	}, Events{
		OnState: func(s State, _ string) { states <- s },
		OnLog:   func(l string) { t.Log("core:", l) },
		OnProbe: func(ok bool, ms int64, msg string) { t.Logf("probe ok=%v %dms %s", ok, ms, msg); probes <- ok },
	})

	if err := eng.Start(); err != nil {
		t.Fatal(err)
	}
	defer eng.Stop()

	if !waitState(states, StateConnected, 15*time.Second) {
		t.Fatal("did not reach connected state")
	}

	// exercise the SOCKS5 server end-to-end (2-hop SSH → local HTTP target)
	resp, err := socksGet(fmt.Sprintf("127.0.0.1:%d", socksPort), targetHost, targetPort, "/")
	if err != nil {
		t.Fatalf("socks GET: %v", err)
	}
	if !strings.HasPrefix(resp, "HTTP/1.1 204") {
		t.Fatalf("unexpected response via SOCKS: %q", resp)
	}

	// and the periodic probe should report reachable
	select {
	case ok := <-probes:
		if !ok {
			t.Fatal("probe reported unreachable")
		}
	case <-time.After(12 * time.Second):
		t.Fatal("no probe result")
	}
}
