// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc_test

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"go.uber.org/goleak"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/bmc"
)

func TestMain(m *testing.M) { goleak.VerifyTestMain(m) }

// mapDialer sends every BMC to the one fake; the fake tells them apart by Host, as the MAT mock does.
type mapDialer string

func (m mapDialer) Address(netip.Addr) string { return string(m) }

func root(id string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) { _, _ = io.WriteString(w, `{"@odata.id":"`+id+`"}`) }
}

var ip1 = netip.MustParseAddr("10.0.0.1")

// TestCheckOneBMC proves each pass and failure of one BMC, how often it is tried, and that the request carries the
// BMC's IP as Host: the fake answers 404 to any other Host, so a mis-set Host fails every passing row.
func TestCheckOneBMC(t *testing.T) {
	var flaky atomic.Int32
	for _, tc := range []struct {
		name    string
		handler http.HandlerFunc
		detail  string // "" passes
		calls   int32
	}{
		{"root", root("/redfish/v1"), "", 1},
		{"root with a slash", root("/redfish/v1/"), "", 1},
		{"404 is not tried again", http.NotFound, "status 404", 1},
		{"503 is tried again", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(503) }, "status 503", 2},
		{"a second try passes", func(w http.ResponseWriter, r *http.Request) {
			if flaky.Add(1) == 1 {
				w.WriteHeader(503)
				return
			}
			root("/redfish/v1")(w, r)
		}, "", 2},
		{"not JSON", func(w http.ResponseWriter, _ *http.Request) { _, _ = io.WriteString(w, "<html>") }, "not a Redfish service root", 1},
		{"another root", root("/redfish/v2"), "not a Redfish service root", 1},
		{"oversized", func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, `{"@odata.id":"/redfish/v1"}`+strings.Repeat(" ", 64<<10))
		}, "reply over 64 KiB", 1},
		{"redirect", func(w http.ResponseWriter, r *http.Request) {
			http.Redirect(w, r, "/redfish/v1/", http.StatusTemporaryRedirect)
		}, "redirect refused", 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var calls atomic.Int32
			srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				if r.Host != "10.0.0.1" || r.URL.Path != "/redfish/v1" {
					http.NotFound(w, r)
					return
				}
				tc.handler(w, r)
			}))
			defer srv.Close()
			got := bmc.Check(context.Background(), []netip.Addr{ip1}, mapDialer(srv.Listener.Addr().String()), bmc.Settings{}, time.Now().Add(10*time.Second))
			switch {
			case tc.detail == "" && len(got) != 0:
				t.Errorf("failures = %+v, want none", got)
			case tc.detail != "" && (len(got) != 1 || got[0] != bmc.Failure{IP: ip1, Detail: tc.detail}):
				t.Errorf("failures = %+v, want %q", got, tc.detail)
			}
			if calls.Load() != tc.calls {
				t.Errorf("calls = %d, want %d", calls.Load(), tc.calls)
			}
		})
	}
}

// TestCheckUnreachableAndDeadline proves a closed port fails after its tries, a BMC that never answers is cut at the
// deadline, not its 5 s try, and a deadline already passed leaves every BMC unchecked, sorted by IP.
func TestCheckUnreachableAndDeadline(t *testing.T) {
	stall := httptest.NewTLSServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) { <-r.Context().Done() }))
	defer stall.Close()
	start := time.Now()
	got := bmc.Check(context.Background(), []netip.Addr{ip1}, mapDialer(stall.Listener.Addr().String()), bmc.Settings{}, start.Add(200*time.Millisecond))
	if len(got) != 1 || !got[0].Unchecked || time.Since(start) > 2*time.Second {
		t.Errorf("stalled BMC: %+v after %v, want unchecked at the deadline", got, time.Since(start))
	}
	srv := httptest.NewTLSServer(root("/redfish/v1"))
	closed := mapDialer(srv.Listener.Addr().String())
	srv.Close()
	ips := []netip.Addr{netip.MustParseAddr("10.0.0.2"), ip1}
	got = bmc.Check(context.Background(), ips[:1], closed, bmc.Settings{}, time.Now().Add(10*time.Second))
	if len(got) != 1 || got[0] != (bmc.Failure{IP: ips[0], Detail: "unreachable"}) {
		t.Errorf("closed port: %+v", got)
	}
	got = bmc.Check(context.Background(), ips, closed, bmc.Settings{}, time.Now().Add(-time.Second))
	want := []bmc.Failure{{ip1, "not checked before the readiness deadline", true}, {ips[0], "not checked before the readiness deadline", true}}
	if len(got) != 2 || got[0] != want[0] || got[1] != want[1] {
		t.Errorf("passed deadline: %+v, want %+v", got, want)
	}
}

// cancelAtEOF cancels the run as the reply body ends, once every byte of the root was read.
type cancelAtEOF struct {
	io.ReadCloser
	cancel context.CancelFunc
}

func (b cancelAtEOF) Read(p []byte) (int, error) {
	n, err := b.ReadCloser.Read(p)
	if err == io.EOF {
		b.cancel()
	}
	return n, err
}

type roundTrip func(*http.Request) (*http.Response, error)

func (f roundTrip) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

// TestCheckTakesNoRootReadAfterACancel proves a valid service root whose body ends after the run was cancelled does
// not pass: the BMC is unchecked, on its last try too.
func TestCheckTakesNoRootReadAfterACancel(t *testing.T) {
	srv := httptest.NewTLSServer(root("/redfish/v1"))
	defer srv.Close()
	for _, tries := range []uint8{1, 2} {
		ctx, cancel := context.WithCancel(context.Background())
		restore := bmc.Wrap(func(t *http.Transport) http.RoundTripper {
			return roundTrip(func(r *http.Request) (*http.Response, error) {
				resp, err := t.RoundTrip(r)
				if err == nil {
					resp.Body = cancelAtEOF{resp.Body, cancel}
				}
				return resp, err
			})
		})
		got := bmc.Check(ctx, []netip.Addr{ip1}, mapDialer(srv.Listener.Addr().String()), bmc.Settings{Tries: tries}, time.Now().Add(10*time.Second))
		restore()
		if len(got) != 1 || got[0] != (bmc.Failure{IP: ip1, Detail: "not checked: cancelled", Unchecked: true}) {
			t.Errorf("%d tries: failures = %+v, want the BMC unchecked", tries, got)
		}
	}
}

// lateDone is a live context whose deadline passes once *passed is set; its Done never fires, as when it is late.
type lateDone struct {
	context.Context
	passed *atomic.Bool
}

func (c lateDone) Deadline() (time.Time, bool) {
	if c.passed.Load() {
		return time.Now().Add(-time.Second), true
	}
	return time.Now().Add(2 * time.Second), true
}

// TestCheckHonoursAPassedDeadlineBeforeItsDone proves a passed deadline whose Done has not fired dispatches no try, and
// a valid root read after the deadline passed does not pass: either way the BMC is unchecked, on its last try too.
func TestCheckHonoursAPassedDeadlineBeforeItsDone(t *testing.T) {
	for _, tc := range []struct {
		during bool
		tries  uint8
		calls  int32
	}{{false, 2, 0}, {true, 2, 1}, {true, 1, 1}} {
		var calls atomic.Int32
		passed := new(atomic.Bool)
		passed.Store(!tc.during)
		restore := bmc.Wrap(func(*http.Transport) http.RoundTripper {
			return roundTrip(func(r *http.Request) (*http.Response, error) {
				calls.Add(1)
				passed.Store(true)
				return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"@odata.id":"/redfish/v1"}`)), Request: r}, nil
			})
		})
		got := bmc.Check(lateDone{context.Background(), passed}, []netip.Addr{ip1}, mapDialer("127.0.0.1:1"), bmc.Settings{Tries: tc.tries}, time.Now().Add(10*time.Second))
		restore()
		if want := (bmc.Failure{IP: ip1, Detail: "not checked before the readiness deadline", Unchecked: true}); len(got) != 1 || got[0] != want || calls.Load() != tc.calls {
			t.Errorf("%+v: %+v after %d calls, want %+v", tc, got, calls.Load(), want)
		}
	}
}

// TestCheckManyBMCs proves 2,000 BMCs, each told apart by Host, pass well inside the deadline.
func TestCheckManyBMCs(t *testing.T) {
	ips, want := make([]netip.Addr, 2000), map[string]bool{}
	for i := range ips {
		ips[i] = netip.AddrFrom4([4]byte{10, 1, byte(i >> 8), byte(i)})
		want[ips[i].String()] = true
	}
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !want[r.Host] {
			http.NotFound(w, r)
			return
		}
		root("/redfish/v1")(w, r)
	}))
	defer srv.Close()
	start := time.Now()
	if got := bmc.Check(context.Background(), ips, mapDialer(srv.Listener.Addr().String()), bmc.Settings{}, start.Add(60*time.Second)); len(got) != 0 {
		t.Fatalf("%d failures, first %+v", len(got), got[0])
	}
	t.Logf("2000 BMCs in %v", time.Since(start))
}
