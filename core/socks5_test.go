package core

import (
	"bytes"
	"encoding/binary"
	"io"
	"net"
	"testing"
	"time"
)

// fakeDialer ignores the requested address and always connects to `to`, standing
// in for the SSH chain so SOCKS paths can be tested without a real tunnel.
type fakeDialer struct{ to string }

func (f fakeDialer) DialTCP(string) (net.Conn, error) {
	return net.DialTimeout("tcp", f.to, 2*time.Second)
}

func TestUDPDatagramRoundTrip(t *testing.T) {
	for _, host := range []string{"1.1.1.1", "2606:4700:4700::1111", "dns.example"} {
		payload := []byte{0xDE, 0xAD, 0xBE, 0xEF}
		dg := buildUDPDatagram(host, 53, payload)
		gotHost, gotPort, gotPayload, ok := parseUDPDatagram(dg)
		if !ok {
			t.Fatalf("%s: parse failed", host)
		}
		if gotPort != 53 || !bytes.Equal(gotPayload, payload) {
			t.Fatalf("%s: port=%d payload=%x", host, gotPort, gotPayload)
		}
		// IP hosts normalise through net.IP; domain must survive verbatim.
		if host == "dns.example" && gotHost != host {
			t.Fatalf("host mangled: %q", gotHost)
		}
	}
	if _, _, _, ok := parseUDPDatagram([]byte{0, 0, 0x01, 0x01}); ok {
		t.Fatal("fragmented datagram (FRAG=1) should be rejected")
	}
}

// TestUDPAssociateDNSoverTCP drives the full UDP ASSOCIATE flow: a UDP DNS query
// sent to the relay must come back having been round-tripped as DNS-over-TCP
// (RFC 7766 length framing) through the (faked) chain.
func TestUDPAssociateDNSoverTCP(t *testing.T) {
	// Fake DNS-over-TCP upstream: read 2-byte length + query, reply framed.
	want := []byte{0x12, 0x34, 0x84, 0x00} // arbitrary "response" bytes
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				var l [2]byte
				if _, err := io.ReadFull(c, l[:]); err != nil {
					return
				}
				q := make([]byte, binary.BigEndian.Uint16(l[:]))
				if _, err := io.ReadFull(c, q); err != nil {
					return
				}
				var out [2]byte
				binary.BigEndian.PutUint16(out[:], uint16(len(want)))
				c.Write(append(out[:], want...))
			}()
		}
	}()

	srv, err := startSocks("127.0.0.1:0", fakeDialer{to: ln.Addr().String()}, func(s string) { t.Log("socks:", s) })
	if err != nil {
		t.Fatal(err)
	}
	defer srv.close()

	// SOCKS5 greeting + UDP ASSOCIATE over the TCP control connection.
	ctrl, err := net.DialTimeout("tcp", srv.addr(), 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer ctrl.Close()
	ctrl.SetDeadline(time.Now().Add(5 * time.Second))
	ctrl.Write([]byte{0x05, 0x01, 0x00})
	if _, err := io.ReadFull(ctrl, make([]byte, 2)); err != nil {
		t.Fatal(err)
	}
	// ASSOCIATE, dst 0.0.0.0:0 (client doesn't know its source yet)
	ctrl.Write([]byte{0x05, cmdUDPAssociate, 0x00, 0x01, 0, 0, 0, 0, 0, 0})
	rep := make([]byte, 10)
	if _, err := io.ReadFull(ctrl, rep); err != nil {
		t.Fatal(err)
	}
	if rep[1] != 0x00 {
		t.Fatalf("associate rejected: code %d", rep[1])
	}
	relayPort := binary.BigEndian.Uint16(rep[8:10])
	relay := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: int(relayPort)}

	uc, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer uc.Close()
	uc.SetDeadline(time.Now().Add(5 * time.Second))

	query := []byte{0xAB, 0xCD, 0x01, 0x00} // stand-in DNS query
	if _, err := uc.WriteToUDP(buildUDPDatagram("1.1.1.1", 53, query), relay); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, maxUDPDatagram)
	n, _, err := uc.ReadFromUDP(buf)
	if err != nil {
		t.Fatalf("no UDP reply: %v", err)
	}
	_, port, payload, ok := parseUDPDatagram(buf[:n])
	if !ok || port != 53 {
		t.Fatalf("bad reply header (ok=%v port=%d)", ok, port)
	}
	if !bytes.Equal(payload, want) {
		t.Fatalf("payload = %x, want %x", payload, want)
	}
}

// Non-DNS UDP must be dropped (no reply), not proxied.
func TestUDPNonDNSDropped(t *testing.T) {
	srv, err := startSocks("127.0.0.1:0", fakeDialer{to: "127.0.0.1:1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer srv.close()

	ctrl, err := net.DialTimeout("tcp", srv.addr(), 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer ctrl.Close()
	ctrl.SetDeadline(time.Now().Add(5 * time.Second))
	ctrl.Write([]byte{0x05, 0x01, 0x00})
	io.ReadFull(ctrl, make([]byte, 2))
	ctrl.Write([]byte{0x05, cmdUDPAssociate, 0x00, 0x01, 0, 0, 0, 0, 0, 0})
	rep := make([]byte, 10)
	if _, err := io.ReadFull(ctrl, rep); err != nil {
		t.Fatal(err)
	}
	relay := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: int(binary.BigEndian.Uint16(rep[8:10]))}

	uc, _ := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	defer uc.Close()
	// dst port 443 (QUIC) — must be dropped
	uc.WriteToUDP(buildUDPDatagram("1.1.1.1", 443, []byte{1, 2, 3}), relay)
	uc.SetDeadline(time.Now().Add(600 * time.Millisecond))
	if _, _, err := uc.ReadFromUDP(make([]byte, 1024)); err == nil {
		t.Fatal("expected no reply for non-DNS UDP")
	}
}
