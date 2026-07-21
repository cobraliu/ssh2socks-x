package core

import (
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"strconv"
	"sync"
	"time"
)

// SOCKS5 server (RFC 1928), no-auth. Supports CONNECT (dialed through the SSH
// chain) and a restricted UDP ASSOCIATE used only for DNS: UDP :53 datagrams are
// translated to DNS-over-TCP (RFC 7766) and dialed through the chain, since SSH
// `direct-tcpip` cannot carry arbitrary UDP. Non-DNS UDP is dropped, so QUIC
// (UDP/443) falls back to TCP TLS instead of silently black-holing.
type socksServer struct {
	ln     net.Listener
	dialer tcpDialer
	logf   func(string)
	wg     sync.WaitGroup
}

// tcpDialer opens TCP connections out through the SSH chain. *Dialer implements
// it; tests inject a fake to exercise the SOCKS paths without a real chain.
type tcpDialer interface {
	DialTCP(addr string) (net.Conn, error)
}

const (
	cmdConnect      = 0x01
	cmdUDPAssociate = 0x03
	dnsTCPTimeout   = 10 * time.Second
	maxUDPDatagram  = 64 * 1024
)

func startSocks(listenAddr string, dialer tcpDialer, logf func(string)) (*socksServer, error) {
	ln, err := net.Listen("tcp", listenAddr)
	if err != nil {
		return nil, err
	}
	s := &socksServer{ln: ln, dialer: dialer, logf: logf}
	s.wg.Add(1)
	go s.acceptLoop()
	return s, nil
}

func (s *socksServer) addr() string { return s.ln.Addr().String() }

func (s *socksServer) acceptLoop() {
	defer s.wg.Done()
	for {
		conn, err := s.ln.Accept()
		if err != nil {
			return // listener closed
		}
		go s.handle(conn)
	}
}

func (s *socksServer) close() {
	_ = s.ln.Close()
	s.wg.Wait()
}

func (s *socksServer) log(msg string) {
	if s.logf != nil {
		s.logf(msg)
	}
}

func (s *socksServer) handle(client net.Conn) {
	defer client.Close()
	cmd, target, err := s.handshake(client)
	if err != nil {
		s.log("SOCKS: " + err.Error())
		return
	}
	switch cmd {
	case cmdConnect:
		s.handleConnect(client, target)
	case cmdUDPAssociate:
		s.handleUDPAssociate(client)
	}
}

func (s *socksServer) handleConnect(client net.Conn, target string) {
	remote, err := s.dialer.DialTCP(target)
	if err != nil {
		writeReply(client, 0x05) // connection refused
		s.log(fmt.Sprintf("拨号 %s 失败: %v", target, err))
		return
	}
	defer remote.Close()
	writeReply(client, 0x00) // succeeded

	done := make(chan struct{}, 2)
	go func() { io.Copy(remote, client); done <- struct{}{} }()
	go func() { io.Copy(client, remote); done <- struct{}{} }()
	<-done
}

// handleUDPAssociate binds a loopback UDP relay, tells the client where to send
// datagrams, and relays DNS (only) over TCP through the SSH chain. The relay
// lives as long as the TCP control connection stays open (RFC 1928 §7).
func (s *socksServer) handleUDPAssociate(client net.Conn) {
	uconn, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		writeReply(client, 0x01)
		s.log("UDP 中继监听失败: " + err.Error())
		return
	}
	defer uconn.Close()

	la := uconn.LocalAddr().(*net.UDPAddr)
	writeUDPReply(client, la)

	// Closing the TCP control connection ends the association: unblock the relay.
	go func() {
		io.Copy(io.Discard, client)
		uconn.Close()
	}()

	s.udpRelayLoop(uconn)
}

func (s *socksServer) udpRelayLoop(uconn *net.UDPConn) {
	buf := make([]byte, maxUDPDatagram)
	for {
		n, cliAddr, err := uconn.ReadFromUDP(buf)
		if err != nil {
			return // relay closed
		}
		host, port, payload, ok := parseUDPDatagram(buf[:n])
		if !ok {
			continue
		}
		if port != 53 {
			// Arbitrary UDP can't ride SSH direct-tcpip; drop it. QUIC/UDP-443
			// clients fall back to TCP. Other UDP apps simply won't work in v1.
			continue
		}
		// Copy the payload out of the shared buffer for the goroutine.
		q := make([]byte, len(payload))
		copy(q, payload)
		go s.relayDNS(uconn, cliAddr, host, port, q)
	}
}

func (s *socksServer) relayDNS(uconn *net.UDPConn, cliAddr *net.UDPAddr, host string, port int, query []byte) {
	resp, err := s.dnsOverTCP(net.JoinHostPort(host, strconv.Itoa(port)), query)
	if err != nil {
		s.log(fmt.Sprintf("DNS(%s) 经 TCP 失败: %v", host, err))
		return
	}
	_, _ = uconn.WriteToUDP(buildUDPDatagram(host, port, resp), cliAddr)
}

// dnsOverTCP sends one DNS query to addr over a TCP connection dialed through
// the SSH chain, using the 2-byte length framing of RFC 7766, and returns the
// response message (framing stripped).
func (s *socksServer) dnsOverTCP(addr string, query []byte) ([]byte, error) {
	conn, err := s.dialer.DialTCP(addr)
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	// ssh channels don't honour SetDeadline; enforce a timeout by closing.
	timer := time.AfterFunc(dnsTCPTimeout, func() { conn.Close() })
	defer timer.Stop()

	var lp [2]byte
	binary.BigEndian.PutUint16(lp[:], uint16(len(query)))
	if _, err := conn.Write(append(lp[:], query...)); err != nil {
		return nil, err
	}
	var rl [2]byte
	if _, err := io.ReadFull(conn, rl[:]); err != nil {
		return nil, err
	}
	resp := make([]byte, binary.BigEndian.Uint16(rl[:]))
	if _, err := io.ReadFull(conn, resp); err != nil {
		return nil, err
	}
	return resp, nil
}

// handshake performs the greeting + request. It returns the command byte and,
// for CONNECT, the "host:port" target. For UDP ASSOCIATE the parsed address is
// the client's (ignored); we relay from whatever source sends us datagrams.
func (s *socksServer) handshake(c net.Conn) (byte, string, error) {
	buf := make([]byte, 262)
	if _, err := io.ReadFull(c, buf[:2]); err != nil {
		return 0, "", err
	}
	if buf[0] != 0x05 {
		return 0, "", fmt.Errorf("非 SOCKS5 请求")
	}
	nmethods := int(buf[1])
	if _, err := io.ReadFull(c, buf[:nmethods]); err != nil {
		return 0, "", err
	}
	// reply: no authentication required
	if _, err := c.Write([]byte{0x05, 0x00}); err != nil {
		return 0, "", err
	}
	// request: VER CMD RSV ATYP ...
	if _, err := io.ReadFull(c, buf[:4]); err != nil {
		return 0, "", err
	}
	cmd := buf[1]
	if cmd != cmdConnect && cmd != cmdUDPAssociate {
		writeReply(c, 0x07) // command not supported
		return 0, "", fmt.Errorf("不支持的命令 0x%02x（仅 CONNECT / UDP ASSOCIATE）", cmd)
	}
	host, err := readAddr(c, buf, buf[3])
	if err != nil {
		if err == errBadAddrType {
			writeReply(c, 0x08) // address type not supported
		}
		return 0, "", err
	}
	if _, err := io.ReadFull(c, buf[:2]); err != nil {
		return 0, "", err
	}
	port := binary.BigEndian.Uint16(buf[:2])
	return cmd, net.JoinHostPort(host, strconv.Itoa(int(port))), nil
}

var errBadAddrType = fmt.Errorf("不支持的地址类型")

// readAddr reads a SOCKS5 address of the given ATYP from c into scratch buf.
func readAddr(c net.Conn, buf []byte, atyp byte) (string, error) {
	switch atyp {
	case 0x01: // IPv4
		if _, err := io.ReadFull(c, buf[:4]); err != nil {
			return "", err
		}
		return net.IP(buf[:4]).String(), nil
	case 0x04: // IPv6
		if _, err := io.ReadFull(c, buf[:16]); err != nil {
			return "", err
		}
		return net.IP(buf[:16]).String(), nil
	case 0x03: // domain
		if _, err := io.ReadFull(c, buf[:1]); err != nil {
			return "", err
		}
		n := int(buf[0])
		if _, err := io.ReadFull(c, buf[:n]); err != nil {
			return "", err
		}
		return string(buf[:n]), nil
	default:
		return "", errBadAddrType
	}
}

// parseUDPDatagram decodes a SOCKS5 UDP request header (RSV RSV FRAG ATYP ADDR
// PORT DATA) and returns the destination and payload. Fragmented datagrams
// (FRAG != 0) are unsupported and rejected.
func parseUDPDatagram(b []byte) (host string, port int, payload []byte, ok bool) {
	if len(b) < 4 || b[2] != 0x00 {
		return "", 0, nil, false
	}
	i := 4
	switch b[3] {
	case 0x01:
		if len(b) < i+4+2 {
			return "", 0, nil, false
		}
		host = net.IP(b[i : i+4]).String()
		i += 4
	case 0x04:
		if len(b) < i+16+2 {
			return "", 0, nil, false
		}
		host = net.IP(b[i : i+16]).String()
		i += 16
	case 0x03:
		if len(b) < i+1 {
			return "", 0, nil, false
		}
		n := int(b[i])
		i++
		if len(b) < i+n+2 {
			return "", 0, nil, false
		}
		host = string(b[i : i+n])
		i += n
	default:
		return "", 0, nil, false
	}
	port = int(binary.BigEndian.Uint16(b[i : i+2]))
	i += 2
	return host, port, b[i:], true
}

// buildUDPDatagram wraps payload in a SOCKS5 UDP reply header addressed from the
// given host:port so the client associates the reply with its query.
func buildUDPDatagram(host string, port int, payload []byte) []byte {
	out := []byte{0x00, 0x00, 0x00}
	ip := net.ParseIP(host)
	switch {
	case ip != nil && ip.To4() != nil:
		out = append(out, 0x01)
		out = append(out, ip.To4()...)
	case ip != nil:
		out = append(out, 0x04)
		out = append(out, ip.To16()...)
	default:
		out = append(out, 0x03, byte(len(host)))
		out = append(out, host...)
	}
	var p [2]byte
	binary.BigEndian.PutUint16(p[:], uint16(port))
	out = append(out, p[:]...)
	return append(out, payload...)
}

// writeReply sends a SOCKS5 reply with the given status and a zero bind addr.
func writeReply(c net.Conn, status byte) {
	_, _ = c.Write([]byte{0x05, status, 0x00, 0x01, 0, 0, 0, 0, 0, 0})
}

// writeUDPReply sends a successful ASSOCIATE reply carrying the relay's bound
// loopback address/port for the client to send its UDP datagrams to.
func writeUDPReply(c net.Conn, a *net.UDPAddr) {
	ip4 := a.IP.To4()
	if ip4 == nil {
		ip4 = net.IPv4(127, 0, 0, 1).To4()
	}
	reply := []byte{0x05, 0x00, 0x00, 0x01}
	reply = append(reply, ip4...)
	var p [2]byte
	binary.BigEndian.PutUint16(p[:], uint16(a.Port))
	reply = append(reply, p[:]...)
	_, _ = c.Write(reply)
}
