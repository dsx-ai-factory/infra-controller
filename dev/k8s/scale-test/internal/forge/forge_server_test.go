// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package forge

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

// fakeForge is a scripted Forge server: it records the method, returns a configurable status and attaches a pushback trailer when set.
type fakeForge struct {
	corev1.UnimplementedForgeServer
	mu       sync.Mutex
	calls    []string
	version  string
	err      error
	pushback string
	block    chan struct{} // when set, a reply waits on it
}

func (f *fakeForge) record(name string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, name)
}

func (f *fakeForge) seen() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.calls...)
}

func (f *fakeForge) reply(ctx context.Context, name string) (*emptypb.Empty, error) {
	f.record(name)
	if f.pushback != "" {
		_ = grpc.SetTrailer(ctx, metadata.Pairs("grpc-retry-pushback-ms", f.pushback))
	}
	if f.block != nil {
		select {
		case <-f.block:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	return &emptypb.Empty{}, f.err
}

func (f *fakeForge) Version(ctx context.Context, _ *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	if _, err := f.reply(ctx, "Version"); err != nil {
		return nil, err
	}
	return &corev1.BuildInfo{BuildVersion: f.version}, nil
}

func (f *fakeForge) AddExpectedRackGroup(ctx context.Context, _ *corev1.ExpectedRackGroup) (*emptypb.Empty, error) {
	return f.reply(ctx, "AddExpectedRackGroup")
}

func (f *fakeForge) AddExpectedRack(ctx context.Context, _ *corev1.ExpectedRack) (*emptypb.Empty, error) {
	return f.reply(ctx, "AddExpectedRack")
}

func (f *fakeForge) AddExpectedMachine(ctx context.Context, _ *corev1.ExpectedMachine) (*emptypb.Empty, error) {
	return f.reply(ctx, "AddExpectedMachine")
}

func (f *fakeForge) AddExpectedSwitch(ctx context.Context, _ *corev1.ExpectedSwitch) (*emptypb.Empty, error) {
	return f.reply(ctx, "AddExpectedSwitch")
}

func (f *fakeForge) AddExpectedPowerShelf(ctx context.Context, _ *corev1.ExpectedPowerShelf) (*emptypb.Empty, error) {
	return f.reply(ctx, "AddExpectedPowerShelf")
}

// serverTLS is a CA, a server cert it signs for forge.test, and the CA PEM.
func serverTLS(t *testing.T) (tls.Certificate, []byte) {
	t.Helper()
	notBefore, notAfter := time.Now().Add(-time.Hour), time.Now().Add(time.Hour)
	caKey, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	caTmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1), IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign,
		NotBefore: notBefore, NotAfter: notAfter,
	}
	caDER, _ := x509.CreateCertificate(rand.Reader, caTmpl, caTmpl, &caKey.PublicKey, caKey)
	caCert, _ := x509.ParseCertificate(caDER)

	srvKey, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	srvTmpl := &x509.Certificate{
		SerialNumber: big.NewInt(2), DNSNames: []string{"forge.test"},
		KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		NotBefore: notBefore, NotAfter: notAfter,
	}
	srvDER, _ := x509.CreateCertificate(rand.Reader, srvTmpl, caCert, &srvKey.PublicKey, caKey)
	cert := tls.Certificate{Certificate: [][]byte{srvDER}, PrivateKey: srvKey}
	return cert, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: caDER})
}

// fixture serves fake over TLS and returns a dialed client and its cert dir. caPEM is what the client trusts (a wrong CA proves verification); the server accepts a
// client with or without a certificate, as nico-api does.
func fixture(t *testing.T, fake *fakeForge, caPEM []byte, serverName string) (*Client, string) {
	t.Helper()
	serverCert, realCA := serverTLS(t)
	if caPEM == nil {
		caPEM = realCA
	}
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	creds := credentials.NewTLS(&tls.Config{Certificates: []tls.Certificate{serverCert}, ClientAuth: tls.RequestClientCert})
	s := grpc.NewServer(grpc.Creds(creds))
	corev1.RegisterForgeServer(s, fake)
	go func() { _ = s.Serve(lis) }()
	t.Cleanup(s.Stop)

	dir := t.TempDir()
	certPEM, keyPEM, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/machine-a-tron", nil)
	for name, data := range map[string][]byte{"tls.crt": certPEM, "tls.key": keyPEM, "ca.crt": caPEM} {
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	c, err := Dial("https://"+lis.Addr().String(), Settings{CertDir: dir, Spiffe: settings, ServerName: serverName})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c, dir
}

// forgeMethod drives one Client RPC with the caller's CallOptions.
type forgeMethod struct {
	name string
	call func(context.Context, *Client, ...grpc.CallOption) error
}

func forgeMethods() []forgeMethod {
	return []forgeMethod{
		{"Version", func(ctx context.Context, c *Client, o ...grpc.CallOption) error {
			_, err := c.Version(ctx, o...)
			return err
		}},
		{"AddExpectedRackGroup", func(ctx context.Context, c *Client, o ...grpc.CallOption) error {
			return c.AddExpectedRackGroup(ctx, &corev1.ExpectedRackGroup{}, o...)
		}},
		{"AddExpectedRack", func(ctx context.Context, c *Client, o ...grpc.CallOption) error {
			return c.AddExpectedRack(ctx, &corev1.ExpectedRack{}, o...)
		}},
		{"AddExpectedMachine", func(ctx context.Context, c *Client, o ...grpc.CallOption) error {
			return c.AddExpectedMachine(ctx, &corev1.ExpectedMachine{}, o...)
		}},
		{"AddExpectedSwitch", func(ctx context.Context, c *Client, o ...grpc.CallOption) error {
			return c.AddExpectedSwitch(ctx, &corev1.ExpectedSwitch{}, o...)
		}},
		{"AddExpectedPowerShelf", func(ctx context.Context, c *Client, o ...grpc.CallOption) error {
			return c.AddExpectedPowerShelf(ctx, &corev1.ExpectedPowerShelf{}, o...)
		}},
	}
}

func TestForgeRPCs(t *testing.T) {
	ctx := context.Background()
	methods := forgeMethods()

	t.Run("every method routes to its RPC and forwards the caller's trailer", func(t *testing.T) {
		for _, m := range methods {
			fake := &fakeForge{version: "v2026.1", pushback: "2500"}
			c, _ := fixture(t, fake, nil, "forge.test")
			var md metadata.MD
			is(t, errText(m.call(ctx, c, grpc.Trailer(&md))), "", m.name+" ok")
			is(t, fake.seen(), []string{m.name}, m.name+" routed")
			is(t, strings.Join(md.Get("grpc-retry-pushback-ms"), ","), "2500", m.name+" trailer")
		}
		fake := &fakeForge{version: "v2026.1"}
		c, _ := fixture(t, fake, nil, "forge.test")
		v, _ := c.Version(ctx)
		is(t, v, "v2026.1", "Version returns the build")
	})

	t.Run("a failed call keeps its code with no server text", func(t *testing.T) {
		for _, hint := range []string{"1500", "-1", ""} {
			fake := &fakeForge{err: status.Error(codes.ResourceExhausted, "SERVER-SECRET"), pushback: hint}
			c, _ := fixture(t, fake, nil, "forge.test")
			var md metadata.MD
			err := c.AddExpectedMachine(ctx, &corev1.ExpectedMachine{}, grpc.Trailer(&md))
			is(t, status.Code(err), codes.ResourceExhausted, "code kept")
			if err == nil || strings.Contains(err.Error(), "SERVER-SECRET") {
				t.Errorf("hint %q: server text leaked: %v", hint, err)
			}
			is(t, strings.Join(md.Get("grpc-retry-pushback-ms"), ","), hint, "pushback hint "+hint)
		}
		fake := &fakeForge{err: status.Error(codes.InvalidArgument, "bad")}
		c, _ := fixture(t, fake, nil, "forge.test")
		is(t, status.Code(c.AddExpectedRack(ctx, &corev1.ExpectedRack{})), codes.InvalidArgument, "invalid argument kept")
	})

	t.Run("a wrong CA or server name fails before the server is reached", func(t *testing.T) {
		_, wrongCA := serverTLS(t)
		for _, tc := range []struct {
			name, serverName string
			ca               []byte
		}{
			{"wrong ca", "forge.test", wrongCA},
			{"wrong name", "other.test", nil},
		} {
			fake := &fakeForge{version: "v"}
			c, _ := fixture(t, fake, tc.ca, tc.serverName)
			_, err := c.Version(ctx)
			is(t, []any{err != nil, len(fake.seen())}, []any{true, 0}, tc.name)
		}
	})

	t.Run("a key removed after dial sends nothing anonymously", func(t *testing.T) {
		fake := &fakeForge{version: "v"}
		c, dir := fixture(t, fake, nil, "forge.test")
		// The key is gone, so the per-handshake reload aborts the handshake: nothing reaches the server without a certificate.
		_ = os.Remove(filepath.Join(dir, "tls.key"))
		_, callErr := c.Version(ctx)
		is(t, []any{callErr != nil, len(fake.seen())}, []any{true, 0}, "no anonymous call")
	})

	t.Run("a reloaded certificate with a wrong SPIFFE identity is refused", func(t *testing.T) {
		fake := &fakeForge{version: "v"}
		c, dir := fixture(t, fake, nil, "forge.test")
		// The mounted pair now names another service. The server accepts any client certificate, so only the client's per-handshake identity check can refuse it;
		// without that check the call would reach the server.
		wrongCert, wrongKey, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/other", nil)
		_ = os.WriteFile(filepath.Join(dir, "tls.crt"), wrongCert, 0o600)
		_ = os.WriteFile(filepath.Join(dir, "tls.key"), wrongKey, 0o600)
		_, callErr := c.Version(ctx)
		is(t, []any{callErr != nil, len(fake.seen())}, []any{true, 0}, "wrong identity refused")
	})

	t.Run("a caller cancel stops a call", func(t *testing.T) {
		fake := &fakeForge{block: make(chan struct{})}
		c, _ := fixture(t, fake, nil, "forge.test")
		cancelled, cancel := context.WithCancel(ctx)
		cancel()
		is(t, status.Code(c.AddExpectedMachine(cancelled, &corev1.ExpectedMachine{})), codes.Canceled, "cancel")
		close(fake.block)
	})

	t.Run("each wrapper bounds a stalled RPC by its own timeout", func(t *testing.T) {
		// The server blocks under a caller deadline far longer than the client's bound: a method bounded only via the caller returns late.
		for _, m := range methods {
			fake := &fakeForge{block: make(chan struct{})}
			c, _ := fixture(t, fake, nil, "forge.test")
			c.timeout = 50 * time.Millisecond
			caller, cancel := context.WithTimeout(ctx, 3*time.Second)
			start := time.Now()
			is(t, status.Code(m.call(caller, c)), codes.DeadlineExceeded, m.name+" bounded")
			if elapsed := time.Since(start); elapsed > time.Second {
				t.Errorf("%s returned after %s, not bounded by its own timeout", m.name, elapsed)
			}
			cancel()
			close(fake.block)
		}
	})
}
