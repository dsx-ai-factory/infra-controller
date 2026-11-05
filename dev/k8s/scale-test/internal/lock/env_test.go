// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build envtest

package lock

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"go.uber.org/goleak"
	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

func suffix() string { // a short random tag, so each run's Lease name is unique
	var b [5]byte
	_, _ = rand.Read(b[:])
	return hex.EncodeToString(b[:])
}

// deleteClient forces HTTP/1.1 with no idle conn, leaving no HTTP/2 read loop.
func deleteClient(cfg *rest.Config) (*http.Client, error) {
	c := *cfg
	c.WrapTransport = func(rt http.RoundTripper) http.RoundTripper {
		t, ok := rt.(*http.Transport)
		if !ok {
			return rt
		}
		t = t.Clone()
		t.DisableKeepAlives = true
		if t.TLSClientConfig != nil {
			t.TLSClientConfig.NextProtos = []string{"http/1.1"}
		}
		return t
	}
	return rest.HTTPClientFor(&c)
}

// deleteLease deletes name over cfg, bounded by ctx. A uid adds a precondition on it (a Lease remade since is left); no uid deletes unconditionally.
func deleteLease(ctx context.Context, cfg *rest.Config, ns, name, uid, version string) error {
	client, err := deleteClient(cfg)
	if err != nil {
		return err
	}
	base, _, err := rest.DefaultServerUrlFor(cfg)
	if err != nil {
		return err
	}
	url := fmt.Sprintf("%s/apis/coordination.k8s.io/v1/namespaces/%s/leases/%s", strings.TrimSuffix(base.String(), "/"), ns, name)
	var body io.Reader
	if uid != "" {
		opts := fmt.Sprintf(`{"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"resourceVersion":%q,"uid":%q}}`, version, uid)
		body = strings.NewReader(opts)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, url, body)
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != 200 && resp.StatusCode != 404 && resp.StatusCode != 409 {
		return fmt.Errorf("delete lease: %d", resp.StatusCode)
	}
	return nil
}

// env is the pod's own in-cluster API server (its ServiceAccount) in the
// namespace SCALE_TEST_NAMESPACE names. Meant to run from the runner Job.
var env struct {
	once sync.Once
	cfg  *rest.Config
	ns   string
	err  error
}

func TestMain(m *testing.M) {
	code := m.Run()
	http.DefaultTransport.(*http.Transport).CloseIdleConnections()
	if err := goleak.Find(goleak.IgnoreAnyFunction("k8s.io/klog/v2.(*flushDaemon).run.func1")); err != nil && code == 0 {
		fmt.Println(err)
		code = 1
	}
	os.Exit(code)
}

func upstream(t *testing.T) (*rest.Config, string) {
	env.once.Do(func() {
		env.cfg, env.err = rest.InClusterConfig()
		env.ns = os.Getenv("SCALE_TEST_NAMESPACE")
	})
	// An acceptance fixture fails, never skips, when its inputs are missing:
	// a skipped Job exits 0 and the chart run reports a false pass.
	if env.err != nil {
		t.Fatal(env.err)
	}
	if env.cfg == nil || env.ns == "" {
		t.Fatal("run in the cluster with SCALE_TEST_NAMESPACE set")
	}
	return env.cfg, env.ns
}

// The proxy readers below serve the real-API scenes only, so they live with
// them under the envtest tag: the connection-reuse test never counts writes.

func (p *proxy) Made() string { p.mu.Lock(); defer p.mu.Unlock(); return p.made }

// count is how many logged writes start with write.
func (p *proxy) count(write string) int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(slices.DeleteFunc(slices.Clone(p.log), func(w string) bool { return !strings.HasPrefix(w, write) }))
}

// countRV is how many PUTs submitted resourceVersion version.
func (p *proxy) countRV(version string) int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(slices.DeleteFunc(slices.Clone(p.puts), func(v string) bool { return v != version }))
}

// scene is a lock through the fault proxy, and api straight to the server.
type scene struct {
	proxy *proxy
	lock  *SiteLock
	api   *cluster.Kube
	up    *rest.Config
	ns    string
	// owned holds the UID of every Lease this scene acquired; cleanup deletes
	// only those, so a foreign or failed-acquire Lease is left.
	owned map[string]bool
}

// take acquires for r, closes it at test end, owns the acquire's own UID.
func (s *scene) take(t *testing.T, r HolderRecord) (Held, error) {
	h, err := s.lock.Acquire(bg, r)
	if err != nil {
		return nil, err
	}
	t.Cleanup(h.Close)
	s.owned[h.(*held).uid] = true
	return h, nil
}

func (s *scene) mustTake(t *testing.T, r HolderRecord) Held {
	h, err := s.take(t, r)
	if err != nil {
		t.Fatal(err)
	}
	return h
}

func (s *scene) lease(t *testing.T) *cluster.Lease {
	l, err := s.api.GetLease(bg, s.lock.Name)
	if err != nil {
		t.Fatal(err)
	}
	return l
}

// remove deletes the lock's Lease, bounded, no precondition: an operator action.
func (s *scene) remove(t *testing.T) {
	ctx, cancel := context.WithTimeout(bg, cluster.CallTimeout)
	defer cancel()
	if err := deleteLease(ctx, s.up, s.ns, s.lock.Name, "", ""); err != nil {
		t.Error(err)
	}
}

// cleanup removes the Lease only when this scene owns its current UID.
func (s *scene) cleanup() {
	ctx, cancel := context.WithTimeout(bg, cluster.CallTimeout)
	defer cancel()
	if l, err := s.api.GetLease(ctx, s.lock.Name); err == nil && l != nil && s.owned[l.UID] {
		_ = deleteLease(ctx, s.up, s.ns, s.lock.Name, l.UID, l.ResourceVersion)
	}
}

// closedWithin is how long the gate took to close, polled, or -1 if still open.
func closedWithin(g *WriteGate, limit time.Duration) time.Duration {
	for started := time.Now(); time.Since(started) < limit; time.Sleep(100 * time.Millisecond) {
		if g.Check() != nil {
			return time.Since(started)
		}
	}
	return -1
}

func pod(run, execution string) HolderRecord {
	return HolderRecord{Kind: HolderTest, RunID: run, ExecutionID: execution, State: Running}
}

// TestEnvLock runs the lock scenarios against a real API server.
func TestEnvLock(t *testing.T) {
	up, ns := upstream(t)
	fast := Timing{2 * time.Second, 20 * time.Second, 8 * time.Second, 2 * time.Second}
	for _, tc := range []struct {
		name   string
		timing Timing
		run    func(t *testing.T, s *scene)
	}{
		{"take-keep", fast, func(t *testing.T, s *scene) {
			h := s.mustTake(t, pod("e2e-take", "pod-a"))
			v, err := s.lock.Read(bg)
			is(t, [4]any{h.Gate().Check(), err, v.Live, v.Record.State}, [4]any{nil, nil, true, Running}, "taken")
			renewed := map[time.Time]bool{s.lease(t).RenewTime: true}
			for range 4 {
				time.Sleep(2500 * time.Millisecond)
				renewed[s.lease(t).RenewTime] = true
			}
			is(t, len(renewed) >= 4, true, "renewTime moves on each look")
			is(t, h.Gate().Check(), nil, "open past the 8 s max renew age")
			is(t, [2]any{h.Release(bg, Final), s.lease(t).HolderIdentity}, [2]any{nil, ""}, "released")
		}},
		{"refuse-second", fast, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-first", "pod-a"))
			_, err := s.take(t, pod("e2e-second", "pod-b"))
			is(t, refusal(err).Reason, ReasonHeldLive, "second refused")
			is(t, [2]any{s.lease(t).HolderIdentity, a.Gate().Check()}, [2]any{"test/e2e-first/pod-a", nil}, "first kept")
			is(t, a.Release(bg, Final), nil, "release")
		}},
		{"sticky", fast, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-sticky", "pod-a"))
			is(t, a.SetState(bg, NeedsCleanup), nil, "set")
			is(t, [3]Reason{refusal(a.SetState(bg, Running)).Reason, refusal(a.SetState(bg, Final)).Reason, refusal(a.Release(bg, Final)).Reason},
				[3]Reason{ReasonNeedsCleanup, ReasonNeedsCleanup, ReasonNeedsCleanup}, "refused")
			l := s.lease(t)
			is(t, [2]any{l.HolderIdentity, stateOn(l)}, [2]any{"test/e2e-sticky/pod-a", NeedsCleanup}, "kept")
			a.Close()
			_, err := s.take(t, pod("e2e-other", "pod-b"))
			is(t, refusal(err).Reason, ReasonNeedsCleanup, "new runner refused")
			time.Sleep(25 * time.Second) // past the 20 s lease and 2 s skew
			_, err = s.take(t, pod("e2e-other", "pod-b"))
			is(t, refusal(err).Reason, ReasonNeedsCleanup, "still refused after expiry")
			s.remove(t)
			b, err := s.take(t, pod("e2e-other", "pod-b"))
			is(t, err, nil, "taken after the operator deletes the Lease")
			is(t, b.Release(bg, Final), nil, "release")
		}},
		{"release", fast, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-release", "pod-a"))
			is(t, a.Release(bg, Final), nil, "release")
			v, err := s.lock.Read(bg)
			is(t, [4]any{s.lease(t).HolderIdentity, a.Gate().Check(), v, err}, [4]any{"", ErrLost, (*HolderView)(nil), nil}, "released")
			b := s.mustTake(t, pod("e2e-next", "pod-b"))
			is(t, b.Release(bg, Final), nil, "next runner")
		}},
		{"lose-other", fast, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-lose", "pod-a"))
			l := *s.lease(t)
			l.HolderIdentity = "test/intruder/pod-x"
			l.Annotations, _ = annotate(l.Annotations, pod("intruder", "pod-x"))
			_, err := s.api.ReplaceLease(bg, l)
			is(t, err, nil, "intruder write")
			seized := s.lease(t)
			is(t, closedWithin(a.Gate(), 10*time.Second) >= 0, true, "the gate closes within a renewal")
			time.Sleep(5 * time.Second)
			is(t, [2]any{a.SetState(bg, Running), a.Release(bg, Final)}, [2]any{ErrLost, ErrLost}, "lost")
			is(t, [2]any{s.lease(t), s.proxy.count("PUT 409")}, [2]any{seized, 1}, "the lost holder never writes again")
		}},
		{"lose-deleted", fast, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-del", "pod-a"))
			s.remove(t)
			closed := closedWithin(a.Gate(), 10*time.Second)
			is(t, closed >= 0 && closed <= 3*time.Second, true, fmt.Sprint("the gate closes at the next renewal, at ", closed))
			time.Sleep(5 * time.Second)
			is(t, [2]any{a.Release(bg, Final), s.lease(t)}, [2]any{ErrLost, (*cluster.Lease)(nil)}, "never made again")
			is(t, [2]int{s.proxy.count("PUT 409"), s.proxy.count("PUT 201")}, [2]int{1, 0}, "the renewal got 409")
		}},
		{"release-503", DefaultTiming, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-503", "pod-a"))
			s.proxy.mode.Store("put503")
			started := time.Now()
			err := a.Release(bg, Final)
			s.proxy.mode.Store("pass")
			is(t, err, unconfirmed(cluster.Error{Kind: cluster.API, Code: 503, Text: "no known reason"}), "may have landed")
			is(t, [3]any{strings.Contains(err.Error(), "secret"), a.Gate().Check(), s.proxy.count("PUT 503")}, [3]any{false, ErrLost, 3}, "3 tries")
			is(t, [2]any{s.lease(t).HolderIdentity, time.Since(started) < 5*time.Second}, [2]any{"test/e2e-503/pod-a", true}, "still held")
		}},
		{"release-timeout", DefaultTiming, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-timeout", "pod-a"))
			s.proxy.mode.Store("puthang")
			started := time.Now()
			err := a.Release(bg, Final)
			s.proxy.mode.Store("pass")
			is(t, [3]any{err, time.Since(started) < 40*time.Second, s.proxy.count("PUT 0")}, [3]any{unconfirmed(tmo), true, 3}, "bounded")
		}},
		{"renewals-blocked", fast, func(t *testing.T, s *scene) {
			a := s.mustTake(t, pod("e2e-blocked", "pod-a"))
			s.proxy.mode.Store("put503")
			closed := closedWithin(a.Gate(), 20*time.Second)
			s.proxy.mode.Store("pass")
			sent := s.proxy.count("PUT ")
			is(t, closed >= 5*time.Second && closed <= 12*time.Second, true, fmt.Sprint("closed by itself at ", closed))
			time.Sleep(4 * time.Second)
			is(t, [2]any{a.SetState(bg, Running), s.proxy.count("PUT ")-sent <= 1}, [2]any{ErrLost, true}, "at most the one give-up write after the cutoff")
		}},
		{"strip-uid", fast, func(t *testing.T, s *scene) {
			// Without the UID the real API makes the deleted Lease again and answers 201: the holder loses the lock and leaves the stray Lease for an operator.
			a := s.mustTake(t, pod("e2e-strip", "pod-a"))
			s.proxy.mode.Store("stripuid")
			s.remove(t)
			is(t, closedWithin(a.Gate(), 10*time.Second) >= 0, true, "lost")
			// Own the remade Lease by the UID its 201 reply stored, so cleanup deletes this scene's own leftover and no later write's.
			s.owned[s.proxy.Made()] = true
			l := s.lease(t)
			if l == nil {
				t.Fatal("the remade Lease is gone")
			}
			is(t, [3]any{s.proxy.count("PUT 201"), cleared(l), a.SetState(bg, Running)}, [3]any{1, false, ErrLost}, "made again and left")
		}},
		{"drain", fast, func(t *testing.T, s *scene) {
			// Drain expiry seals the gate, then releases needsCleanup.
			a := s.mustTake(t, pod("e2e-drain", "pod-a"))
			a.Gate().Seal()
			is(t, a.Release(bg, NeedsCleanup), nil, "release")
			l := s.lease(t)
			is(t, [2]any{stateOn(l), l.HolderIdentity}, [2]any{NeedsCleanup, "test/e2e-drain/pod-a"}, "needsCleanup persisted")
		}},
		{"acquire-post-lost-reply", fast, func(t *testing.T, s *scene) {
			// The create commits (201) but its reply is dropped; acquire reads back by this attempt's token and holds, no replay, no second owner.
			s.proxy.mode.Store("commitdrop")
			h := s.mustTake(t, pod("e2e-lostpost", "pod-a"))
			// Exactly one create: the one dropped 201, and no passed POST, so the acquisition write was read back by token, never replayed.
			is(t, [4]any{h.Gate().Check(), s.lease(t).HolderIdentity, s.proxy.count("dropPOST 201"), s.proxy.count("POST ")},
				[4]any{nil, "test/e2e-lostpost/pod-a", 1, 0}, "one dropped 201 create, no replay, held by read-back")
			_, err := s.take(t, pod("e2e-other", "pod-b"))
			is(t, [2]any{refusal(err).Reason, h.Release(bg, Final)}, [2]any{ReasonHeldLive, nil}, "no second owner")
		}},
		{"acquire-put-lost-reply", fast, func(t *testing.T, s *scene) {
			// A released Lease is taken by a replace whose reply is dropped. The count is scoped to the version the acquisition submits, so a renewal (a later version) never makes one write two, and a replay would.
			a := s.mustTake(t, pod("e2e-first", "pod-a"))
			is(t, a.Release(bg, Final), nil, "released")
			rv0 := s.lease(t).ResourceVersion // the version the acquisition replace submits
			s.proxy.mode.Store("commitdrop")
			h := s.mustTake(t, pod("e2e-lostput", "pod-b"))
			is(t, [4]any{h.Gate().Check(), s.lease(t).HolderIdentity, s.proxy.countRV(rv0), s.proxy.count("dropPUT 200")},
				[4]any{nil, "test/e2e-lostput/pod-b", 1, 1}, "one acquisition PUT, dropped at 200, held by read-back")
			is(t, h.Release(bg, Final), nil, "release")
		}},
		{"foreign-uid-survives", fast, func(t *testing.T, s *scene) {
			// A Lease remade under this name (a UID this scene never took) survives.
			a := s.mustTake(t, pod("e2e-own", "pod-a"))
			a.Close()
			s.remove(t) // the operator deletes the owned Lease
			made, err := s.api.CreateLease(bg, cluster.Lease{Name: s.lock.Name, HolderIdentity: "test/foreign/pod-z", LeaseDurationSeconds: 60, AcquireTime: time.Now().UTC()})
			is(t, err, nil, "another run remade it")
			s.cleanup() // owns only the deleted UID, so it must not touch this one
			l := s.lease(t)
			is(t, [2]any{l != nil, l.UID}, [2]any{true, made.UID}, "the unowned replacement survives")
			ctx, cancel := context.WithTimeout(bg, cluster.CallTimeout)
			defer cancel()
			_ = deleteLease(ctx, s.up, s.ns, s.lock.Name, made.UID, l.ResourceVersion) // teardown of the fixture's own Lease
		}},
		{"failed-acquire-keeps-holder", fast, func(t *testing.T, s *scene) {
			// A live foreign holder: acquire is refused, so cleanup owns nothing.
			foreign := pod("foreign", "pod-z")
			ann, _ := annotate(nil, foreign)
			made, err := s.api.CreateLease(bg, cluster.Lease{Name: s.lock.Name, HolderIdentity: foreign.Identity(),
				LeaseDurationSeconds: 60, AcquireTime: time.Now().UTC(), RenewTime: time.Now().UTC(), Annotations: ann})
			is(t, err, nil, "foreign holder")
			_, aerr := s.take(t, pod("e2e-us", "pod-a"))
			is(t, refusal(aerr).Reason, ReasonHeldLive, "refused")
			s.cleanup()
			l := s.lease(t)
			is(t, [2]any{l != nil, l.UID}, [2]any{true, made.UID}, "the foreign holder survives a failed acquire")
			ctx, cancel := context.WithTimeout(bg, cluster.CallTimeout)
			defer cancel()
			_ = deleteLease(ctx, s.up, s.ns, s.lock.Name, made.UID, l.ResourceVersion)
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			p, through := newProxy(t, up)
			viaProxy, err := cluster.NewFor(through, ns)
			if err != nil {
				t.Fatal(err)
			}
			api, err := cluster.NewFor(up, ns)
			if err != nil {
				t.Fatal(err)
			}
			s := &scene{p, &SiteLock{viaProxy, "e2e-go-" + tc.name + "-" + suffix(), tc.timing}, api, up, ns, map[string]bool{}}
			t.Cleanup(s.cleanup)
			tc.run(t, s)
		})
	}
}

// TestEnvCleanupIsBounded proves a cleanup delete is bounded by its context, so a hung server never blocks the run.
func TestEnvCleanupIsBounded(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		<-r.Context().Done() // never answer
	}))
	defer srv.Close()
	ctx, cancel := context.WithTimeout(bg, 300*time.Millisecond)
	defer cancel()
	start := time.Now()
	err := deleteLease(ctx, &rest.Config{Host: srv.URL}, "ns", "l", "", "")
	if err == nil || time.Since(start) > 5*time.Second {
		t.Errorf("unbounded: %v after %v", err, time.Since(start))
	}
}
