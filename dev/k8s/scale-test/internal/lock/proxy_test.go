// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package lock

import (
	"bytes"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

// proxy sits in front of the API. A mode (pass, put503, puthang, stripuid,
// commitdrop) applies to Lease writes. It logs each as "<method> <status>"
// ("drop<method>" for a dropped reply) and each PUT's submitted
// resourceVersion, so a count can scope to one request and skip renewals.
type proxy struct {
	mode atomic.Value
	mu   sync.Mutex
	log  []string
	puts []string // submitted resourceVersion of each PUT lease write
	made string   // UID of the Lease the last 201 reply stored
}

func (p *proxy) add(method string, status int) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.log = append(p.log, fmt.Sprint(method, " ", status))
}

func (p *proxy) setMade(uid string) { p.mu.Lock(); defer p.mu.Unlock(); p.made = uid }

func (p *proxy) addPut(version string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.puts = append(p.puts, version)
}

func leaseWrite(r *http.Request) bool {
	return strings.Contains(r.URL.Path, "/leases") && (r.Method == http.MethodPut || r.Method == http.MethodPost)
}

func newProxy(t *testing.T, up *rest.Config) (*proxy, *rest.Config) {
	target, _, err := rest.DefaultServerUrlFor(up)
	if err != nil {
		t.Fatal(err)
	}
	// A fresh HTTP/1.1 connection per upstream write, closed after. A pooled or
	// HTTP/2-multiplexed connection to the API server drops under the parallel
	// scene burst, and the proxied POST/PUT carries no GetBody to rewind on the
	// retry, which the server surfaces as a 502. DisableKeepAlives removes the
	// reuse and NextProtos keeps ALPN off HTTP/2, so no write is ever replayed.
	// TLS and bearer auth still come from the config.
	tlsConfig, err := rest.TLSConfigFor(up)
	if err != nil {
		t.Fatal(err)
	}
	if tlsConfig != nil {
		tlsConfig.NextProtos = []string{"http/1.1"}
	}
	transport, err := rest.HTTPWrappersForConfig(up, &http.Transport{
		TLSClientConfig:   tlsConfig,
		DisableKeepAlives: true,
		Proxy:             http.ProxyFromEnvironment,
	})
	if err != nil {
		t.Fatal(err)
	}
	p := &proxy{}
	p.mode.Store("pass")
	forward := &httputil.ReverseProxy{
		Rewrite:   func(r *httputil.ProxyRequest) { r.SetURL(target) },
		Transport: transport,
		ModifyResponse: func(resp *http.Response) error {
			if leaseWrite(resp.Request) {
				p.add(resp.Request.Method, resp.StatusCode)
				if resp.StatusCode == 201 {
					body, _ := io.ReadAll(resp.Body)
					resp.Body, resp.ContentLength = io.NopCloser(bytes.NewReader(body)), int64(len(body))
					var o map[string]any
					_ = json.Unmarshal(body, &o)
					meta, _ := o["metadata"].(map[string]any)
					uid, _ := meta["uid"].(string)
					p.setMade(uid)
				}
			}
			return nil
		},
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if leaseWrite(r) && r.Method == http.MethodPut {
			body, _ := io.ReadAll(r.Body)
			r.Body = io.NopCloser(bytes.NewReader(body))
			var o map[string]any
			_ = json.Unmarshal(body, &o)
			meta, _ := o["metadata"].(map[string]any)
			rv, _ := meta["resourceVersion"].(string)
			p.addPut(rv)
		}
		switch mode := p.mode.Load(); {
		case !leaseWrite(r):
		case mode == "put503":
			p.add(r.Method, 503)
			w.WriteHeader(503)
			_, _ = io.WriteString(w, `{"kind":"Status","message":"fault-proxy-secret-body","code":503}`)
			return
		case mode == "puthang":
			p.add(r.Method, 0)
			_, _ = io.Copy(io.Discard, r.Body) // so the server sees the client leave
			<-r.Context().Done()
			return
		case mode == "stripuid" && r.Method == http.MethodPut:
			var o map[string]any
			_ = json.NewDecoder(r.Body).Decode(&o)
			delete(o["metadata"].(map[string]any), "uid")
			body, _ := json.Marshal(o)
			r.Body, r.ContentLength = io.NopCloser(bytes.NewReader(body)), int64(len(body))
		case mode == "commitdrop":
			// One shot: forward so the write commits, then close the client connection so the client sees a lost reply it must read back.
			p.mode.Store("pass")
			body, _ := io.ReadAll(r.Body)
			u := *target
			u.Path, u.RawQuery = r.URL.Path, r.URL.RawQuery
			req, _ := http.NewRequestWithContext(r.Context(), r.Method, u.String(), bytes.NewReader(body))
			req.Header, req.Close = r.Header.Clone(), true // a fresh connection, closed after
			status := 0
			if resp, err := transport.RoundTrip(req); err == nil {
				status = resp.StatusCode
				_ = resp.Body.Close()
			}
			p.add("drop"+r.Method, status) // a dropped reply, distinct from a passed write
			if conn, _, err := w.(http.Hijacker).Hijack(); err == nil {
				_ = conn.Close()
			}
			return
		}
		forward.ServeHTTP(w, r)
	}))
	t.Cleanup(srv.Close)
	return p, &rest.Config{Host: srv.URL}
}

// obs records each upstream write's HTTP major version and counts the
// connections the upstream accepts, so a fresh connection per write shows up
// without depending on reused local ports.
type obs struct {
	mu       sync.Mutex
	protos   []int
	newConns int
}

func (o *obs) record(r *http.Request) {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.protos = append(o.protos, r.ProtoMajor)
}

func (o *obs) onState(s http.ConnState) {
	if s == http.StateNew {
		o.mu.Lock()
		o.newConns++
		o.mu.Unlock()
	}
}

func (o *obs) snapshot() (protos []int, newConns int) {
	o.mu.Lock()
	defer o.mu.Unlock()
	return slices.Clone(o.protos), o.newConns
}

// h2Upstream is a trusted TLS API server that also offers HTTP/2, recording the
// protocol of every Lease write and the connections it accepts.
func h2Upstream(t *testing.T) (*rest.Config, *obs) {
	o := &obs{}
	srv := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if leaseWrite(r) {
			o.record(r)
		}
		_, _ = io.WriteString(w, `{"kind":"Lease","metadata":{"name":"l","resourceVersion":"1","uid":"u1"}}`)
	}))
	srv.EnableHTTP2 = true
	srv.Config.ConnState = func(_ net.Conn, s http.ConnState) { o.onState(s) }
	srv.StartTLS()
	t.Cleanup(srv.Close)
	ca := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: srv.Certificate().Raw})
	return &rest.Config{Host: srv.URL, TLSClientConfig: rest.TLSClientConfig{CAData: ca}}, o
}

// writeN sends n Lease writes one after another through the proxy config,
// failing on any error.
func writeN(t *testing.T, through *rest.Config, n int) {
	k, err := cluster.NewFor(through, "ns")
	if err != nil {
		t.Fatal(err)
	}
	l := cluster.Lease{Name: "l", ResourceVersion: "1", UID: "u1"}
	for range n {
		if _, err := k.ReplaceLease(bg, l); err != nil {
			t.Fatalf("write: %v", err)
		}
	}
}

// oldProxy rebuilds the original fault proxy's upstream path: a pooled,
// HTTP/2-capable transport that only closes a connection after its reply. It is
// the negative control the fix must beat.
func oldProxy(t *testing.T, up *rest.Config) *rest.Config {
	target, _, err := rest.DefaultServerUrlFor(up)
	if err != nil {
		t.Fatal(err)
	}
	transport, err := rest.TransportFor(up)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(&httputil.ReverseProxy{
		Rewrite:   func(r *httputil.ProxyRequest) { r.SetURL(target); r.Out.Close = true },
		Transport: transport,
	})
	t.Cleanup(srv.Close)
	return &rest.Config{Host: srv.URL}
}

// TestEnvProxyForcesFreshHTTP1Upstream proves the fault proxy reaches the API
// server over a fresh HTTP/1.1 connection per write, never a pooled or
// HTTP/2-multiplexed one that drops under the parallel burst and forces an
// unrewindable replay (the 502 seen on a dev cluster). Against a trusted TLS
// server that also offers HTTP/2, a burst of writes must each arrive as
// HTTP/1.1, the upstream must accept one new connection per write, and see
// exactly the writes sent with no replay. The original pooled transport is the
// negative control: it reaches the same server over HTTP/2, the multiplexed
// protocol the fix replaces.
func TestEnvProxyForcesFreshHTTP1Upstream(t *testing.T) {
	const n = 4
	up, seen := h2Upstream(t)
	_, through := newProxy(t, up)
	writeN(t, through, n)

	protos, newConns := seen.snapshot()
	if len(protos) != n {
		t.Fatalf("upstream saw %d writes, want exactly %d (a replay duplicates one)", len(protos), n)
	}
	for _, p := range protos {
		if p != 1 {
			t.Errorf("upstream write used HTTP/%d, want HTTP/1.1", p)
		}
	}
	if newConns != n {
		t.Errorf("upstream accepted %d connections for %d writes, want a fresh one each", newConns, n)
	}

	// Negative control: the original pooled transport reaches the same server
	// over HTTP/2, whose dropped connection forced the replay. Forcing HTTP/1.1
	// is the fix.
	up2, seen2 := h2Upstream(t)
	writeN(t, oldProxy(t, up2), n)
	ctrl, _ := seen2.snapshot()
	if len(ctrl) != n {
		t.Fatalf("negative control upstream saw %d writes, want %d", len(ctrl), n)
	}
	for _, p := range ctrl {
		if p != 2 {
			t.Errorf("negative control write used HTTP/%d, want HTTP/2 to reproduce the bug", p)
		}
	}
}
