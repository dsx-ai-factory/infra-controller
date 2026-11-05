// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package lock

import (
	"context"
	"encoding/json"
	"errors"
	"math"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/clustertest"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

const name = "nico-scale-test-site-lock"

// t0 is when every synctest bubble starts.
var (
	t0        = time.Date(2000, 1, 1, 0, 0, 0, 0, time.UTC)
	bg        = context.Background()
	tmo       = cluster.Error{Kind: cluster.Timeout, Secs: 10}
	down      = cluster.Error{Kind: cluster.Transport, Text: "the connection failed"}
	throttled = cluster.Error{Kind: cluster.API, Code: 429, Text: "TooManyRequests"}
	forbidden = cluster.Error{Kind: cluster.Forbidden, Text: "Forbidden"}
	conflict  = cluster.Error{Kind: cluster.Conflict}
)

func is(t *testing.T, got, want any, what string) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%s: got %v, want %v", what, got, want)
	}
}

func at(secs int) time.Time { return t0.Add(time.Duration(secs) * time.Second) }

// renewedAfter is how long after its acquire the Lease was last renewed.
func renewedAfter(f *clustertest.Fake) time.Duration {
	l := f.Lease(name)
	return l.RenewTime.Sub(l.AcquireTime)
}

func me(run string) HolderRecord {
	return HolderRecord{Kind: HolderTest, RunID: run, ExecutionID: run + "-x", State: Running}
}

func unconfirmed(e cluster.Error) error { return Error{Kind: KindUnconfirmed, Err: e} }

func refusal(err error) Refusal {
	var e Error
	errors.As(err, &e)
	return e.Refusal
}

// setup is a lock on a fake whose every call is bounded, as the adapter
// bounds its own.
func setup() (*clustertest.Fake, *SiteLock) {
	f := clustertest.New("nico-mat")
	f.Bound = cluster.CallTimeout
	return f, &SiteLock{Cluster: f, Name: name, Timing: DefaultTiming}
}

// take acquires for run, and closes what it took when the test ends.
func take(t *testing.T, l *SiteLock, r HolderRecord) (Held, error) {
	h, err := l.Acquire(bg, r)
	if err == nil {
		t.Cleanup(h.Close)
	}
	return h, err
}

func setupHeld(t *testing.T) (*clustertest.Fake, *SiteLock, Held) {
	f, l := setup()
	h, err := take(t, l, me("r1"))
	if err != nil {
		t.Fatal(err)
	}
	return f, l, h
}

// createOrReplace is a lock and the call a new holder's write goes through:
// a create of a missing Lease, or a replace of a released one.
func createOrReplace(t *testing.T, released bool) (*clustertest.Fake, *SiteLock, clustertest.Op) {
	f, l := setup()
	if !released {
		return f, l, clustertest.CreateLease
	}
	h, _ := take(t, l, me("r0"))
	is(t, h.Release(bg, Final), nil, "r0 released")
	return f, l, clustertest.ReplaceLease
}

// onOp runs act whenever op is called.
func onOp(f *clustertest.Fake, op clustertest.Op, act func()) {
	f.OnCall(func(o clustertest.Op, _ string) {
		if o == op {
			act()
		}
	})
}

// nth runs acts[i] at the call i of op; a failure queued there is next's.
func nth(f *clustertest.Fake, op clustertest.Op, acts ...func()) {
	n := 0
	onOp(f, op, func() {
		if n < len(acts) && acts[n] != nil {
			acts[n]()
		}
		n++
	})
}

func count(f *clustertest.Fake, ops ...clustertest.Op) (n int) {
	for _, c := range f.Calls() {
		for _, op := range ops {
			if c.Op == op {
				n++
			}
		}
	}
	return n
}

func writes(f *clustertest.Fake) int {
	return count(f, clustertest.CreateLease, clustertest.ReplaceLease)
}

func opsSince(f *clustertest.Fake, from int) (ops []clustertest.Op) {
	for _, c := range f.Calls()[from:] {
		ops = append(ops, c.Op)
	}
	return ops
}

func stateOf(f *clustertest.Fake) HolderState { return stateOn(f.Lease(name)) }

func stateOn(l *cluster.Lease) HolderState {
	var r HolderRecord
	_ = json.Unmarshal([]byte(l.Annotations[HolderAnnotation]), &r)
	return r.State
}

func lease(r HolderRecord, token string) cluster.Lease {
	a, _ := annotate(nil, r)
	if token != "" {
		a[TokenAnnotation] = token
	}
	return cluster.Lease{Name: name, HolderIdentity: r.Identity(), LeaseDurationSeconds: 60, AcquireTime: t0, RenewTime: t0, Annotations: a}
}

// steal makes another process take the Lease with a new version.
func steal(f *clustertest.Fake) {
	l := *f.Lease(name)
	l.HolderIdentity = me("r9").Identity()
	l.Annotations, _ = annotate(l.Annotations, me("r9"))
	f.SetLease(l)
}

// replaceToken is another acquire of the same record: new token and version.
func replaceToken(f *clustertest.Fake) cluster.Lease {
	l := *f.Lease(name)
	l.Annotations[TokenAnnotation] = "other"
	return f.SetLease(l)
}

// del deletes the Lease at its current version, as an operator would.
func del(f *clustertest.Fake) { _ = f.DeleteLease(name, f.Lease(name).ResourceVersion) }

// cleared reads as released and carries nothing of r1's.
func cleared(l *cluster.Lease) bool {
	return l.HolderIdentity == "" && l.Annotations[HolderAnnotation] == "" && l.Annotations[TokenAnnotation] == ""
}

// apply runs one held-lock write: the 10 s renewal (gate is the outcome), SetState or Release.
func apply(h Held, how string) error {
	switch how {
	case "renew":
		time.Sleep(11 * time.Second)
		return h.Gate().Check()
	case "state":
		return h.SetState(bg, NeedsCleanup)
	}
	return h.Release(bg, Final)
}

func TestDecideTake(t *testing.T) {
	held := func(s HolderState, renewed int) *cluster.Lease {
		l := lease(HolderRecord{Kind: HolderTest, RunID: "r1", ExecutionID: "r1-x", State: s}, "")
		l.AcquireTime, l.RenewTime = at(-3600), at(-renewed)
		return &l
	}
	// 65 s since renewal is past 60 s plus the 5 s skew. 64 s is not.
	foreign, noAnnotation, released := held(Running, 64), held(Running, 64), held(Running, 64)
	foreign.HolderIdentity = "test/r1/someone-else"
	delete(noAnnotation.Annotations, HolderAnnotation)
	released.HolderIdentity = ""
	for _, tc := range []struct {
		name string
		cur  *cluster.Lease
		take string
		r    Reason
	}{
		{"missing", nil, "missing", ""},
		{"released", released, "released", ""},
		{"live test", held(Running, 64), "", ReasonHeldLive},
		{"expired final", held(Final, 66), "expiredFinal", ""},
		{"live final", held(Final, 64), "", ReasonHeldLive},
		{"expired running", held(Running, 66), "", ReasonCrashedRun},
		{"needsCleanup", held(NeedsCleanup, 66), "", ReasonNeedsCleanup},
		{"foreign identity", foreign, "", ReasonForeign},
		{"no holder annotation", noAnnotation, "", ReasonForeign},
	} {
		take, r := decideTake(tc.cur, t0, DefaultTiming)
		is(t, take, tc.take, tc.name)
		is(t, r != nil && r.Reason == tc.r || r == nil && tc.r == "", true, tc.name)
	}
}

// A Check blocked on LeaseWritable's lock still sees a Seal that lands while
// it waits: the seal is read after the lock, so a sealed gate never passes as
// success. Holding the lock, it runs outside a synctest bubble.
func TestSealIsSeenDuringACheck(t *testing.T) {
	g := Open()
	g.mu.Lock()
	errc := make(chan error, 1)
	go func() { errc <- g.Check() }()
	// Block strictly before sealing, so without the fix (sealed read first)
	// the regression fails by logic, not by scheduling luck.
	deadline := time.Now().Add(2 * time.Second)
	for !blockedIn("LeaseWritable") {
		if time.Now().After(deadline) {
			t.Fatal("Check never blocked in LeaseWritable")
		}
		time.Sleep(time.Millisecond)
	}
	g.Seal()
	g.mu.Unlock()
	if err := <-errc; err != error(ErrLost) {
		t.Errorf("got %v, want ErrLost", err)
	}
}

// blockedIn reports whether any goroutine's stack is in fn right now.
func blockedIn(fn string) bool {
	buf := make([]byte, 1<<16)
	return strings.Contains(string(buf[:runtime.Stack(buf, true)]), fn)
}

func TestLock(t *testing.T) {
	bools := []bool{false, true}
	for _, tc := range []struct {
		name string
		run  func(t *testing.T)
	}{
		{"holder record wire names", func(t *testing.T) {
			r := HolderRecord{Kind: HolderFleetUp, RunID: "r1", ExecutionID: "r1-abcde", State: NeedsCleanup}
			is(t, r.Identity(), "fleet-up/r1/r1-abcde", "identity")
			raw, _ := json.Marshal(r)
			is(t, string(raw), `{"kind":"fleet-up","runId":"r1","executionId":"r1-abcde","state":"needsCleanup","test":null}`, "json")
		}},
		{"delayed renewal after cutoff never reopens gate", func(t *testing.T) {
			// A renewal sent at 10 s comes back at 35 s, after the 30 s cutoff.
			unchecked, checked := OpenUntil(at(30)), OpenUntil(at(30))
			time.Sleep(31 * time.Second)
			is(t, checked.Check(), ErrLost, "closed at 30 s")
			time.Sleep(4 * time.Second)
			for _, g := range []*WriteGate{unchecked, checked} {
				g.ExtendUntil(at(40))
				is(t, g.Check(), ErrLost, "late renewal")
			}
			time.Sleep(time.Second)
			is(t, unchecked.Check(), ErrLost, "no check in between, still latched")
		}},
		{"write gate follows renewals and closes for good", func(t *testing.T) {
			g := OpenUntil(at(30))
			is(t, g.Check(), nil, "open")
			time.Sleep(20 * time.Second)
			g.ExtendUntil(at(50))
			g.ExtendUntil(at(20)) // an older renewal never moves the limit back
			time.Sleep(29 * time.Second)
			is(t, g.Check(), nil, "open at 49 s")
			time.Sleep(time.Second)
			is(t, g.Check(), ErrLost, "renewal 30 s old")
			g = Open()
			time.Sleep(time.Hour)
			is(t, g.Check(), nil, "open gate")
			g.Close()
			g.ExtendUntil(time.Now().Add(time.Minute))
			is(t, g.Check(), ErrLost, "closed for good")
		}},
		{"a foreign holder shows no text it could hide a secret in", func(t *testing.T) {
			l := cluster.Lease{HolderIdentity: "https://user:password@example"}
			_, r := holderOf(l, l.HolderIdentity)
			is(t, *r, Refusal{Reason: ReasonForeign, Text: "a holder this lock cannot read"}, "foreign")
			_, r = holderOf(l, "SecretToken123")
			is(t, strings.Contains(refused(*r).Error(), "SecretToken123"), false, "a token of name characters")
			// A matching record shows its kind and run, never its execution.
			secret := HolderRecord{Kind: HolderTest, RunID: "r1", ExecutionID: "SecretToken123", State: Running}
			for _, reason := range []Reason{ReasonHeldLive, ReasonNeedsCleanup, ReasonCrashedRun} {
				is(t, strings.Contains(refused(Refusal{Reason: reason, Holder: secret}).Error(), "SecretToken"), false, string(reason))
			}
			// A readable identity whose record is unreadable or has an extra field.
			secret.ExecutionID = "x?token=secret"
			l = lease(secret, "")
			_, r = holderOf(l, l.HolderIdentity)
			is(t, r.Reason, ReasonForeign, "unreadable execution")
			l = lease(me("r1"), "")
			l.Annotations[HolderAnnotation] = strings.Replace(l.Annotations[HolderAnnotation], "}", `,"extra":1}`, 1)
			_, r = holderOf(l, l.HolderIdentity)
			is(t, r != nil, true, "unknown field")
			l = lease(me("r1"), "")
			l.Annotations[HolderAnnotation] = strings.Replace(l.Annotations[HolderAnnotation], `"running"`, `"paused"`, 1)
			_, r = holderOf(l, l.HolderIdentity)
			is(t, r != nil, true, "unknown state")
			sl := lease(HolderRecord{Kind: HolderTest, RunID: "ci/job", ExecutionID: "r1-x", State: Running}, "")
			_, r = holderOf(sl, sl.HolderIdentity)
			is(t, r != nil && r.Reason == ReasonForeign, true, "ambiguous run id")
		}},
		{"a concurrent taker refuses even when its read fails", func(t *testing.T) {
			// r9 takes the Lease as r1 creates it, and r1's read after fails.
			for _, createTimedOut := range bools {
				f, l := setup()
				if createTimedOut {
					f.FailNext(clustertest.CreateLease, tmo)
				}
				nth(f, clustertest.CreateLease, func() {
					f.SetLease(lease(me("r9"), ""))
					if !createTimedOut {
						f.FailNext(clustertest.GetLease, down)
					}
				})
				_, err := take(t, l, me("r1"))
				if createTimedOut {
					is(t, [2]string{string(refusal(err).Reason), refusal(err).Holder.RunID}, [2]string{"heldLive", "r9"}, "timed out")
				} else {
					is(t, refusal(err), Refusal{Reason: ReasonForeign, Text: "a concurrent taker"}, "refused")
				}
			}
		}},
		{"second run refused while lock held", func(t *testing.T) {
			f, l, h := setupHeld(t)
			_, err := take(t, l, HolderRecord{Kind: HolderIngestion, RunID: "r2", ExecutionID: "r2-x"})
			is(t, [2]string{string(refusal(err).Reason), refusal(err).Holder.RunID}, [2]string{"heldLive", "r1"}, "refusal")
			v, _ := l.Read(bg)
			is(t, [2]any{v.Live, v.Record.RunID}, [2]any{true, "r1"}, "read")
			is(t, f.Lease(name).HolderIdentity, "test/r1/r1-x", "holder")
			is(t, h.Gate().Check(), nil, "gate")
		}},
		{"acquire that commits but loses its reply is held", func(t *testing.T) {
			for _, released := range bools {
				f, l, op := createOrReplace(t, released)
				f.LoseReply(op, tmo)
				h, err := take(t, l, me("r1"))
				if err != nil {
					t.Fatalf("read back as held: %v", err) // stop by name, not a nil-pointer panic below
				}
				is(t, stateOf(f), Running, "state")
				// Renewals use the version read back and keep the gate open.
				time.Sleep(31 * time.Second)
				is(t, h.Gate().Check(), nil, "renewed on the read back")
				is(t, h.Release(bg, Final), nil, "release")
				is(t, f.Lease(name).HolderIdentity, "", "released")
			}
		}},
		{"acquire with a lost reply and no own lease is an error", func(t *testing.T) {
			f, l := setup()
			f.FailNext(clustertest.CreateLease, tmo)
			_, err := take(t, l, me("r1"))
			is(t, [2]any{err != nil, f.Lease(name)}, [2]any{true, (*cluster.Lease)(nil)}, "nothing committed")
			// The write fails, and the read back shows another holder.
			onOp(f, clustertest.CreateLease, func() { f.SetLease(lease(me("r9"), "")) })
			f.FailNext(clustertest.CreateLease, tmo)
			_, err = take(t, l, me("r1"))
			is(t, [2]any{err != nil, f.Lease(name).HolderIdentity}, [2]any{true, "test/r9/r9-x"}, "another holder's")
		}},
		{"concurrent identical holders get one owner", func(t *testing.T) {
			// Both write a missing or released Lease; the slower gets a refusal.
			for _, released := range bools {
				f, l, op := createOrReplace(t, released)
				f.Delay(op, time.Second)
				var wg sync.WaitGroup
				errs := make([]error, 2)
				for i := range errs {
					wg.Go(func() { _, errs[i] = take(t, l, me("r1")) })
				}
				wg.Wait()
				is(t, (errs[0] == nil) != (errs[1] == nil), true, "exactly one owner")
				is(t, refusal(errors.Join(errs[0], errs[1])).Reason, ReasonHeldLive, "the other")
			}
		}},
		{"lost reply needs this attempt's token", func(t *testing.T) {
			// The write never commits; another acquire of the same record writes it.
			f, l := setup()
			onOp(f, clustertest.CreateLease, func() { f.SetLease(lease(me("r1"), "other")) })
			f.FailNext(clustertest.CreateLease, tmo)
			_, err := take(t, l, me("r1"))
			is(t, err != nil, true, "the same record with another token is not ours")
		}},
		{"same holder with another token is never written", func(t *testing.T) {
			for _, how := range []string{"renew", "state", "release"} {
				f, _, h := setupHeld(t)
				other := replaceToken(f)
				is(t, apply(h, how), ErrLost, how)
				is(t, h.Gate().Check(), ErrLost, how+": gate")
				is(t, h.SetState(bg, NeedsCleanup), ErrLost, how+": later write")
				is(t, *f.Lease(name), other, how+": the other Lease is untouched")
			}
		}},
		{"a deleted lease loses the lock at once", func(t *testing.T) {
			// The UID check refuses a write to a deleted Lease, so the re-read
			// finds nothing and the lock is lost with no Lease made again.
			for _, how := range []string{"renew", "state", "release"} {
				f, _, h := setupHeld(t)
				del(f)
				is(t, apply(h, how), ErrLost, how)
				is(t, h.Gate().Check(), ErrLost, how+": gate")
				sent := writes(f)
				time.Sleep(2 * time.Minute)
				is(t, [2]error{h.SetState(bg, Running), h.Release(bg, Final)}, [2]error{ErrLost, ErrLost}, how+": later writes refused")
				is(t, [2]any{writes(f), f.Lease(name)}, [2]any{sent, (*cluster.Lease)(nil)}, how+": nothing made again, no write after")
			}
		}},
		{"a conflict whose read-back finds no lease loses the lock", func(t *testing.T) {
			// A stale cached version conflicts, and the Lease is deleted during
			// the read-back: the lock is lost, independent of the UID check.
			f, _, h := setupHeld(t)
			f.SetLease(*f.Lease(name))
			onOp(f, clustertest.GetLease, func() { del(f) })
			is(t, h.SetState(bg, NeedsCleanup), ErrLost, "lost")
			is(t, [2]any{h.Gate().Check(), f.Lease(name)}, [2]any{ErrLost, (*cluster.Lease)(nil)}, "gate closed, nothing made again")
		}},
		{"a lease made again by another writer is not ours", func(t *testing.T) {
			// An operator deletes the Lease and remakes it: same holder/token, new UID.
			for _, how := range []string{"renew", "state", "release"} {
				f, _, h := setupHeld(t)
				copied := *f.Lease(name)
				del(f)
				again := f.SetLease(copied)
				is(t, apply(h, how), ErrLost, how)
				is(t, h.Gate().Check(), ErrLost, how+": gate")
				is(t, *f.Lease(name), again, how+": the Lease made again is never written")
			}
		}},
		{"an unexpected reply that remade the deleted lease loses the lock", func(t *testing.T) {
			// An API skipping the UID check remakes the deleted Lease (201). The
			// lock is lost and the stray Lease is left for an operator, never
			// cleared or written again.
			f, _, h := setupHeld(t)
			f.SkipUIDCheck()
			del(f)
			is(t, h.SetState(bg, NeedsCleanup), ErrLost, "lost")
			is(t, h.Gate().Check(), ErrLost, "gate")
			sent, made := writes(f), f.Lease(name)
			is(t, made != nil && !cleared(made), true, "the remade Lease is left")
			time.Sleep(2 * time.Minute)
			is(t, h.Release(bg, Final), ErrLost, "release")
			is(t, [2]any{writes(f), f.Lease(name)}, [2]any{sent, made}, "no write after the loss")
		}},
		{"a reply naming another holder loses the lock", func(t *testing.T) {
			// A successful write comes back with this holder's UID but another
			// holder's identity, so the cached Lease no longer names this holder.
			// The next write closes the gate and reports the lock lost.
			f, _, h := setupHeld(t)
			f.ForeignReply()
			is(t, h.SetState(bg, NeedsCleanup), nil, "the wrong reply is taken as written")
			is(t, h.SetState(bg, Running), ErrLost, "the next write loses the lock")
			is(t, h.Gate().Check(), ErrLost, "gate closed")
		}},
		{"close closes the gate and stops renewal", func(t *testing.T) {
			f, _, h := setupHeld(t)
			g := h.Gate()
			h.Close()
			is(t, g.Check(), ErrLost, "closed")
			time.Sleep(time.Minute)
			is(t, writes(f), 1, "only the acquire, no renewal after Close")
			is(t, stateOf(f), Running, "the Lease is left for recovery")
		}},
		{"renewal that commits with a lost reply still releases final", func(t *testing.T) {
			for _, retaken := range bools {
				f, _, h := setupHeld(t)
				token := f.Lease(name).Annotations[TokenAnnotation]
				// The 10 s renewal lands but its reply is lost; the cached version is stale.
				f.LoseReply(clustertest.ReplaceLease, tmo)
				time.Sleep(11 * time.Second)
				is(t, [2]any{renewedAfter(f), h.Gate().Check()}, [2]any{10 * time.Second, nil}, "renewed")
				if retaken {
					replaceToken(f)
				}
				from := len(f.Calls())
				err := h.Release(bg, Final)
				if l := f.Lease(name); retaken {
					// The read back shows the same record with another token.
					is(t, err, ErrLost, "retaken")
					is(t, opsSince(f, from), []clustertest.Op{clustertest.ReplaceLease, clustertest.GetLease}, "retaken ops")
					is(t, [2]any{stateOf(f), l.HolderIdentity}, [2]any{Running, "test/r1/r1-x"}, "untouched")
				} else {
					// Conflict, read back with this exact token, then final.
					is(t, err, nil, "released")
					is(t, opsSince(f, from), []clustertest.Op{clustertest.ReplaceLease, clustertest.GetLease, clustertest.ReplaceLease}, "ops")
					is(t, [3]any{stateOf(f), l.HolderIdentity, l.Annotations[TokenAnnotation]}, [3]any{Final, "", token}, "final")
				}
				is(t, h.Gate().Check(), ErrLost, "gate")
			}
		}},
		{"renewal cancelled by release may still commit", func(t *testing.T) {
			for _, commitsFirst := range bools {
				f, _, h := setupHeld(t)
				f.ParkNext(clustertest.ReplaceLease)
				// The renewal at 10 s is at the server, its reply pending.
				time.Sleep(10 * time.Second)
				synctest.Wait()
				if commitsFirst {
					// The server commits it just before the release's write.
					onOp(f, clustertest.ReplaceLease, func() { _ = f.CommitParked() })
				}
				from, started := len(f.Calls()), time.Now()
				is(t, h.Release(bg, Final), nil, "release")
				is(t, time.Since(started), time.Duration(0), "the renewal was cancelled, not timed out")
				ops, renewed := []clustertest.Op{clustertest.ReplaceLease}, time.Duration(0)
				if commitsFirst {
					// Conflict on the renewal's version, read back, final.
					ops, renewed = append(ops, clustertest.GetLease, clustertest.ReplaceLease), 10*time.Second
				} else {
					// Committed later, the renewal conflicts: final stays.
					is(t, f.CommitParked(), error(conflict), "late renewal")
				}
				is(t, opsSince(f, from), ops, "ops")
				l := f.Lease(name)
				is(t, [3]any{l.HolderIdentity, stateOf(f), renewedAfter(f)}, [3]any{"", Final, renewed}, "final")
				is(t, h.Gate().Check(), ErrLost, "gate")
			}
		}},
		{"acquire that commits after its read back is refused", func(t *testing.T) {
			// The create hangs past the call timeout and the read back finds nothing.
			f, l := setup()
			f.ParkNext(clustertest.CreateLease)
			_, err := take(t, l, me("r1"))
			is(t, err, error(Error{Kind: KindCluster, Err: tmo}), "timed out")
			is(t, opsSince(f, 0), []clustertest.Op{clustertest.GetLease, clustertest.CreateLease, clustertest.GetLease}, "ops")
			// Then it commits: a running Lease no Held renews.
			is(t, f.CommitParked(), nil, "commit")
			orphan := *f.Lease(name)
			is(t, stateOf(f), Running, "orphan")
			time.Sleep(30 * time.Second)
			is(t, [2]any{*f.Lease(name), writes(f)}, [2]any{orphan, 1}, "no renewal")
			// Never taken over, live or expired, even by its own record.
			_, err = take(t, l, me("r1"))
			is(t, [2]string{string(refusal(err).Reason), refusal(err).Holder.RunID}, [2]string{"heldLive", "r1"}, "live")
			time.Sleep(40 * time.Second)
			_, err = take(t, l, me("r2"))
			is(t, [2]string{string(refusal(err).Reason), refusal(err).Holder.RunID}, [2]string{"crashedRun", "r1"}, "expired")
			is(t, *f.Lease(name), orphan, "refusals write nothing")
			// Recovery deletes only the version inspected, then the site is free.
			is(t, f.DeleteLease(name, "0"), error(conflict), "a Lease that changed is kept")
			del(f)
			h, err := take(t, l, me("r2"))
			is(t, [2]any{err, h.Gate().Check()}, [2]any{nil, nil}, "free")
		}},
		{"acquire tokens are random uuids", func(t *testing.T) {
			// Two acquires of the same record, one after the other.
			f, l, h := setupHeld(t)
			first := f.Lease(name).Annotations[TokenAnnotation]
			is(t, h.Release(bg, Final), nil, "release")
			_, err := take(t, l, me("r1"))
			second := f.Lease(name).Annotations[TokenAnnotation]
			for _, token := range []string{first, second} {
				u, perr := uuid.Parse(token)
				is(t, [4]any{err, perr, u.Version(), u.String()}, [4]any{nil, nil, uuid.Version(4), token}, token)
			}
			is(t, first != second, true, "distinct")
		}},
		{"renewal failure closes gate after 30s", func(t *testing.T) {
			f, _, h := setupHeld(t)
			for range 10 {
				f.FailNext(clustertest.ReplaceLease, down)
			}
			time.Sleep(29_900 * time.Millisecond)
			is(t, h.Gate().Check(), nil, "last good renewal under 30 s ago")
			time.Sleep(100 * time.Millisecond)
			is(t, h.Gate().Check(), ErrLost, "closed at 30 s")
			time.Sleep(time.Minute)
			is(t, [2]any{h.Gate().Check(), h.SetState(bg, NeedsCleanup)}, [2]any{ErrLost, ErrLost}, "closed for good")
			// Every write in all: renewals at 10 s and 20 s, then one give-up.
			is(t, count(f, clustertest.ReplaceLease), 3, "the give-up write is tried once")
		}},
		{"renewals keep the gate open", func(t *testing.T) {
			f, _, h := setupHeld(t)
			time.Sleep(301 * time.Second)
			is(t, [3]any{h.Gate().Check(), renewedAfter(f), count(f, clustertest.ReplaceLease)}, [3]any{nil, 300 * time.Second, 30}, "renewed")
		}},
		{"renewal returning after cutoff keeps gate closed", func(t *testing.T) {
			for _, checkInBetween := range bools {
				// A slow API that still answers within its call timeout.
				f, l := setup()
				f.Bound = time.Minute
				h, _ := take(t, l, me("r1"))
				// The renewal sent at 10 s lands, but only at 35 s.
				f.Delay(clustertest.ReplaceLease, 25*time.Second)
				if checkInBetween {
					time.Sleep(31 * time.Second)
					is(t, h.Gate().Check(), ErrLost, "closed at 30 s")
					time.Sleep(5 * time.Second)
				} else {
					time.Sleep(36 * time.Second)
				}
				is(t, h.Gate().Check(), ErrLost, "a late renewal never reopens the gate")
				is(t, renewedAfter(f), 10*time.Second, "the renewal landed")
				is(t, h.SetState(bg, Running), ErrLost, "lost")
			}
		}},
		{"hung renewal is bounded and release returns", func(t *testing.T) {
			f, _, h := setupHeld(t)
			acquired := *f.Lease(name)
			f.Delay(clustertest.ReplaceLease, math.MaxInt64)
			// The renewal at 10 s hangs. Release at 15 s still returns: one try from 15 s to 25 s, one more from 26 s to 36 s, none past the 30 s renewal age.
			time.Sleep(15 * time.Second)
			started := time.Now()
			is(t, h.Release(bg, Final), unconfirmed(tmo), "release")
			is(t, time.Since(started), 21*time.Second, "elapsed")
			is(t, h.Gate().Check(), ErrLost, "no writes after a release attempt")
			time.Sleep(2 * time.Minute)
			is(t, *f.Lease(name), acquired, "nothing was written")
		}},
		{"hung renewal still closes gate on time", func(t *testing.T) {
			f, _, h := setupHeld(t)
			f.Delay(clustertest.ReplaceLease, math.MaxInt64)
			time.Sleep(29_900 * time.Millisecond)
			is(t, h.Gate().Check(), nil, "open")
			time.Sleep(100 * time.Millisecond)
			is(t, h.Gate().Check(), ErrLost, "closed")
			started := time.Now()
			is(t, h.SetState(bg, Running) != nil, true, "refused")
			is(t, time.Since(started) <= 10*time.Second, true, "bounded")
		}},
		{"conflict with other holder loses lock", func(t *testing.T) {
			f, _, h := setupHeld(t)
			steal(f)
			// The next renewal conflicts, re-reads and sees another holder.
			time.Sleep(11 * time.Second)
			is(t, [2]any{h.Gate().Check(), h.SetState(bg, Final)}, [2]any{ErrLost, ErrLost}, "lost at once")
		}},
		{"lost lock never writes other holder's lease", func(t *testing.T) {
			f, _, h := setupHeld(t)
			steal(f)
			stolen := *f.Lease(name)
			time.Sleep(11 * time.Second)
			is(t, count(f, clustertest.ReplaceLease), 1, "one conflicting renewal")
			time.Sleep(2 * time.Minute)
			is(t, [2]any{h.SetState(bg, NeedsCleanup), h.Release(bg, Final)}, [2]any{ErrLost, ErrLost}, "lost")
			is(t, [2]any{count(f, clustertest.ReplaceLease), *f.Lease(name)}, [2]any{1, stolen}, "no write after the loss")
		}},
		{"cas ops serialized and holder checked", func(t *testing.T) {
			// Each replace takes 500 ms, so all ten are in flight at once unless the holder's mutex puts them one after another.
			f, _, h := setupHeld(t)
			f.Delay(clustertest.ReplaceLease, 500*time.Millisecond)
			started := time.Now()
			var wg sync.WaitGroup
			for i := range 10 {
				wg.Go(func() { is(t, h.SetState(bg, []HolderState{Final, Running}[i%2]), nil, "set") })
			}
			wg.Wait()
			is(t, time.Since(started), 5*time.Second, "one at a time")
			is(t, [2]int{count(f, clustertest.GetLease), count(f, clustertest.ReplaceLease)}, [2]int{1, 10}, "no conflict re-reads")
			// Same holder identity, other execution: foreign to this process.
			l, twin := *f.Lease(name), me("r1")
			twin.ExecutionID = "r1-y"
			l.Annotations, _ = annotate(l.Annotations, twin)
			f.SetLease(l)
			is(t, [2]any{h.SetState(bg, Running), h.Gate().Check()}, [2]any{ErrLost, ErrLost}, "twin")
		}},
		{"lost lock sets needsCleanup only on own version", func(t *testing.T) {
			// Renewals at 10 s and 20 s fail, the gate closes at 30 s, then the API is back: the cached version still matches.
			f, _, h := setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, down)
			f.FailNext(clustertest.ReplaceLease, down)
			time.Sleep(41 * time.Second)
			is(t, [2]any{h.Gate().Check(), stateOf(f)}, [2]any{ErrLost, NeedsCleanup}, "recorded")
			// Another process took the Lease meanwhile: the write on the cached version conflicts and is never retried.
			f, _, h = setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, down)
			f.FailNext(clustertest.ReplaceLease, down)
			time.Sleep(25 * time.Second)
			steal(f)
			stolen := *f.Lease(name)
			time.Sleep(20 * time.Second)
			is(t, [2]any{h.Gate().Check(), *f.Lease(name)}, [2]any{ErrLost, stolen}, "untouched")
		}},
		{"needsCleanup lease not taken by test", func(t *testing.T) {
			f, l, h := setupHeld(t)
			is(t, h.Release(bg, NeedsCleanup), nil, "release")
			time.Sleep(2 * time.Minute)
			_, err := take(t, l, me("r2"))
			is(t, [2]any{refusal(err).Reason, stateOf(f)}, [2]any{ReasonNeedsCleanup, NeedsCleanup}, "kept for the operator")
		}},
		{"a sealed gate still lets the release record needsCleanup", func(t *testing.T) {
			// Drain expiry seals the gate first, then releases needsCleanup.
			f, _, h := setupHeld(t)
			h.Gate().Seal()
			is(t, h.Gate().Check(), ErrLost, "sealed")
			is(t, h.Release(bg, NeedsCleanup), nil, "release")
			is(t, [2]any{stateOf(f), f.Lease(name).HolderIdentity}, [2]any{NeedsCleanup, "test/r1/r1-x"}, "persisted")
		}},
		{"bad timing is refused before any write", func(t *testing.T) {
			for _, timing := range []Timing{
				{0, time.Minute, 30 * time.Second, 5 * time.Second},
				{10 * time.Second, time.Minute, math.MaxInt64, 5 * time.Second},
				{10 * time.Second, time.Minute, 90 * time.Second, 5 * time.Second},
				{10 * time.Second, time.Minute, 30 * time.Second, -time.Second},
				// The Lease holds whole seconds: these would not fit it.
				{100 * time.Millisecond, 500 * time.Millisecond, 300 * time.Millisecond, 0},
				{10 * time.Second, 60_500 * time.Millisecond, 30 * time.Second, 5 * time.Second},
			} {
				f, l := setup()
				l.Timing = timing
				_, err := take(t, l, me("r1"))
				var e Error
				is(t, [2]any{errors.As(err, &e) && e.Kind == KindTiming, len(f.Calls())}, [2]any{true, 0}, timing.LeaseDuration.String())
			}
			is(t, DefaultTiming.check(), nil, "default")
		}},
		{"writes stop before the release is sent", func(t *testing.T) {
			f, _, h := setupHeld(t)
			f.ParkNext(clustertest.ReplaceLease)
			released := make(chan error, 1)
			go func() { released <- h.Release(bg, Final) }()
			synctest.Wait()
			// The release may already have freed the Lease with its reply late.
			is(t, h.Gate().Check(), ErrLost, "no write while the release is in flight")
			is(t, f.CommitParked(), nil, "commit")
			is(t, [3]any{<-released, h.Gate().Check(), f.Lease(name).HolderIdentity}, [3]any{nil, ErrLost, ""}, "released")
		}},
		{"a release cancelled at the server is unconfirmed", func(t *testing.T) {
			// The release write waits at the server when its caller cancels, and lands later.
			f, _, h := setupHeld(t)
			f.ParkNext(clustertest.ReplaceLease)
			ctx, cancel := context.WithCancel(bg)
			released := make(chan error, 1)
			go func() { released <- h.Release(ctx, Final) }()
			synctest.Wait()
			cancel()
			is(t, <-released, error(Error{Kind: KindUnconfirmed, Err: context.Canceled}), "release")
			is(t, [2]any{f.CommitParked(), f.Lease(name).HolderIdentity}, [2]any{nil, ""}, "it landed")
		}},
		{"a release is tried again after a transient failure", func(t *testing.T) {
			// The first try fails before or after it commits.
			for _, committed := range bools {
				f, _, h := setupHeld(t)
				if committed {
					f.LoseReply(clustertest.ReplaceLease, tmo)
				} else {
					f.FailNext(clustertest.ReplaceLease, tmo)
				}
				is(t, h.Release(bg, Final), nil, "release")
				is(t, [3]any{f.Lease(name).HolderIdentity, stateOf(f), h.Gate().Check()}, [3]any{"", Final, ErrLost}, "released")
			}
		}},
		{"the lock adds no timeout to the adapter's own", func(t *testing.T) {
			// A slow API answers in 15 s: the adapter's bound decides, not one of the lock's own.
			f, l := setup()
			f.Bound = 0
			f.Delay(clustertest.GetLease, 15*time.Second)
			started := time.Now()
			_, err := take(t, l, me("r1"))
			is(t, [2]any{err, time.Since(started) >= 15*time.Second}, [2]any{nil, true}, "waited")
		}},
		{"close closes the gate during a renewal", func(t *testing.T) {
			f, _, h := setupHeld(t)
			g := h.Gate()
			f.ParkNext(clustertest.ReplaceLease)
			time.Sleep(10 * time.Second)
			synctest.Wait()
			h.Close()
			is(t, g.Check(), ErrLost, "closed while the renewal is parked")
			_ = f.CommitParked()
		}},
		{"a release never settles a recorded needsCleanup", func(t *testing.T) {
			f, _, h := setupHeld(t)
			is(t, h.SetState(bg, NeedsCleanup), nil, "set")
			sent := count(f, clustertest.ReplaceLease)
			// Neither a later state nor the release clears it, and both say so.
			is(t, [2]Reason{refusal(h.SetState(bg, Final)).Reason, refusal(h.Release(bg, Final)).Reason},
				[2]Reason{ReasonNeedsCleanup, ReasonNeedsCleanup}, "refused")
			is(t, [3]any{count(f, clustertest.ReplaceLease), h.Gate().Check(), stateOf(f)}, [3]any{sent, ErrLost, NeedsCleanup}, "nothing written")
			is(t, f.Lease(name).HolderIdentity, "test/r1/r1-x", "the holder stays for an operator")
		}},
		{"a needsCleanup read after the last conflict refuses", func(t *testing.T) {
			// Both tries conflict on a write of the same holder, and the second read back carries needsCleanup.
			f, _, h := setupHeld(t)
			rewrite := func(s HolderState) func() {
				return func() {
					l := *f.Lease(name)
					r := me("r1")
					r.State = s
					l.Annotations, _ = annotate(l.Annotations, r)
					f.SetLease(l)
				}
			}
			nth(f, clustertest.ReplaceLease, rewrite(Running), rewrite(NeedsCleanup))
			is(t, refusal(h.SetState(bg, Final)).Reason, ReasonNeedsCleanup, "refused")
			is(t, stateOf(f), NeedsCleanup, "kept")
		}},
		{"the gate closes before the lease the acquire wrote ends", func(t *testing.T) {
			// A slow read before the write: the Lease times and the gate both start after it.
			f, l := setup()
			l.Timing = Timing{100 * time.Millisecond, time.Second, 900 * time.Millisecond, 0}
			f.Delay(clustertest.GetLease, 1500*time.Millisecond)
			h, _ := take(t, l, me("r1"))
			f.Delay(clustertest.ReplaceLease, math.MaxInt64) // no renewal lands
			time.Sleep(time.Until(f.Lease(name).RenewTime.Add(time.Second)))
			is(t, h.Gate().Check(), ErrLost, "closed when the Lease ends")
		}},
		{"a needsCleanup set during the release pause stays", func(t *testing.T) {
			// Try 1 of the release fails before it lands. In the pause before try 2 the run records needsCleanup.
			f, _, h := setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, tmo)
			released := make(chan error, 1)
			go func() { released <- h.Release(bg, Final) }()
			time.Sleep(500 * time.Millisecond)
			is(t, len(released), 0, "in the pause")
			is(t, h.SetState(bg, NeedsCleanup), nil, "set")
			is(t, refusal(<-released).Reason, ReasonNeedsCleanup, "refused")
			is(t, [3]any{stateOf(f), f.Lease(name).HolderIdentity, h.Gate().Check()}, [3]any{NeedsCleanup, "test/r1/r1-x", ErrLost}, "kept")
		}},
		{"an invalid release reply is unconfirmed and not tried again", func(t *testing.T) {
			// The release lands, and its 2xx reply does not decode.
			f, _, h := setupHeld(t)
			invalid := cluster.Error{Kind: cluster.Invalid, Text: "the reply did not decode"}
			f.LoseReply(clustertest.ReplaceLease, invalid)
			sent := count(f, clustertest.ReplaceLease)
			is(t, h.Release(bg, Final), unconfirmed(invalid), "release")
			is(t, count(f, clustertest.ReplaceLease), sent+1, "one try")
			is(t, [2]bool{transient(invalid), transient(down)}, [2]bool{false, true}, "transient")
		}},
		{"the last or only try may leave the release unconfirmed", func(t *testing.T) {
			// Two throttled tries, then a third that lands with its reply lost.
			f, _, h := setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, throttled)
			nth(f, clustertest.ReplaceLease,
				func() { f.FailNext(clustertest.ReplaceLease, throttled) },
				func() { f.LoseReply(clustertest.ReplaceLease, tmo) })
			is(t, h.Release(bg, Final), unconfirmed(tmo), "third try")
			// One try only: the renewal age passes while it waits.
			f, _, h = setupHeld(t)
			f.Delay(clustertest.ReplaceLease, math.MaxInt64)
			time.Sleep(21 * time.Second)
			started := time.Now()
			is(t, h.Release(bg, Final), unconfirmed(tmo), "only try")
			is(t, time.Since(started), 10*time.Second, "one try")
		}},
		{"a holder the lease cannot carry is refused before any call", func(t *testing.T) {
			colon, unknown, slash := me("r1"), me("r1"), me("r1")
			colon.ExecutionID, unknown.Kind, slash.RunID = "r1:x", "pirate", "ci/job"
			for i, r := range []HolderRecord{colon, unknown, slash} {
				f, l := setup()
				_, err := take(t, l, r)
				var e Error
				is(t, [3]any{errors.As(err, &e) && e.Kind == KindCorrupt, len(f.Calls()), strings.Contains(e.Text, []string{"execution ID", "", "run ID"}[i])}, [3]any{true, 0, true}, r.Identity())
			}
			// A state the lock does not know is refused with no write.
			f, _, h := setupHeld(t)
			corrupt := error(Error{Kind: KindCorrupt, Text: "the holder record does not encode"})
			is(t, [2]error{h.SetState(bg, "paused"), h.Release(bg, "paused")}, [2]error{corrupt, corrupt}, "state")
			is(t, [2]any{writes(f), stateOf(f)}, [2]any{1, Running}, "only the acquire")
		}},
		{"a release lost after an unanswered try is unconfirmed", func(t *testing.T) {
			// The first try commits with its reply lost, or answered 503 by a proxy. Another run then takes the free Lease before the retry.
			for _, lost := range []cluster.Error{tmo, {Kind: cluster.API, Code: 503, Text: "no known reason"}} {
				f, _, h := setupHeld(t)
				f.LoseReply(clustertest.ReplaceLease, lost)
				nth(f, clustertest.ReplaceLease, nil, func() { f.SetLease(lease(me("r9"), "")) })
				is(t, h.Release(bg, Final), unconfirmed(lost), lost.Error())
			}
		}},
		{"a throttled, unavailable or proxy timed out release is tried again", func(t *testing.T) {
			for _, e := range []cluster.Error{throttled, {Kind: cluster.API, Code: 408, Text: "no known reason"}, {Kind: cluster.API, Code: 503, Text: "no known reason"}} {
				f, _, h := setupHeld(t)
				f.FailNext(clustertest.ReplaceLease, e)
				is(t, [2]any{h.Release(bg, Final), f.Lease(name).HolderIdentity}, [2]any{nil, ""}, e.Error())
			}
		}},
		{"a failed read is never the unconfirmed outcome", func(t *testing.T) {
			// Try 1 conflicts and its read back fails, try 2 commits with its reply lost, and another run has the Lease by try 3.
			f, _, h := setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, conflict)
			nth(f, clustertest.ReplaceLease, func() {
				f.FailNext(clustertest.GetLease, down)
				f.LoseReply(clustertest.ReplaceLease, tmo)
			}, nil, func() { f.SetLease(lease(me("r9"), "")) })
			is(t, h.Release(bg, Final), unconfirmed(tmo), "release")
		}},
		{"a throttled try before an unanswered one is not the outcome", func(t *testing.T) {
			// Try 1 is throttled, try 2 commits with its reply lost, try 3 is refused.
			f, _, h := setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, throttled)
			nth(f, clustertest.ReplaceLease,
				func() { f.LoseReply(clustertest.ReplaceLease, tmo) },
				func() { f.FailNext(clustertest.ReplaceLease, forbidden) })
			is(t, h.Release(bg, Final), unconfirmed(tmo), "release")
		}},
		{"an unanswered try then refusals is still unconfirmed", func(t *testing.T) {
			// Try 1 commits with its reply lost, and both retries are throttled, or the second meets a permanent refusal.
			for _, last := range []cluster.Error{throttled, forbidden} {
				f, _, h := setupHeld(t)
				f.LoseReply(clustertest.ReplaceLease, tmo)
				nth(f, clustertest.ReplaceLease,
					func() { f.FailNext(clustertest.ReplaceLease, throttled) },
					func() { f.FailNext(clustertest.ReplaceLease, last) })
				is(t, h.Release(bg, Final), unconfirmed(tmo), "then "+last.Error())
			}
		}},
		{"the first unanswered try decides an unconfirmed release", func(t *testing.T) {
			// Try 1 commits with its reply lost, try 2 is throttled, and another run has the Lease by try 3.
			f, _, h := setupHeld(t)
			f.LoseReply(clustertest.ReplaceLease, tmo)
			nth(f, clustertest.ReplaceLease,
				func() { f.FailNext(clustertest.ReplaceLease, throttled) },
				nil, func() { f.SetLease(lease(me("r9"), "")) })
			is(t, h.Release(bg, Final), unconfirmed(tmo), "release")
		}},
		{"a release is not tried again after a permanent failure", func(t *testing.T) {
			f, _, h := setupHeld(t)
			f.FailNext(clustertest.ReplaceLease, forbidden)
			sent := count(f, clustertest.ReplaceLease)
			is(t, h.Release(bg, Final), error(Error{Kind: KindCluster, Err: forbidden}), "release")
			is(t, count(f, clustertest.ReplaceLease), sent+1, "one try")
		}},
		{"release final frees lock", func(t *testing.T) {
			f, l := setup()
			h, _ := take(t, l, HolderRecord{Kind: HolderFleetUp, RunID: "r1", ExecutionID: "r1-x"})
			is(t, h.Release(bg, Final), nil, "release")
			is(t, [3]any{h.Gate().Check(), f.Lease(name).HolderIdentity, stateOf(f)}, [3]any{ErrLost, "", Final}, "released")
			v, err := l.Read(bg)
			is(t, [2]any{v, err}, [2]any{(*HolderView)(nil), nil}, "read")
			next, err := take(t, l, me("r2"))
			is(t, [2]any{err, next.Gate().Check()}, [2]any{nil, nil}, "next taker")
			// The released lock's renewal is gone: no write touches r2's Lease.
			sent := count(f, clustertest.ReplaceLease)
			time.Sleep(11 * time.Second)
			is(t, count(f, clustertest.ReplaceLease), sent+1, "only r2 renews")
		}},
	} {
		t.Run(tc.name, func(t *testing.T) { synctest.Test(t, tc.run) })
	}
}

// neverStop is a soft-stop signal that never fires.
type neverStop struct{}

func (neverStop) Stopped() <-chan struct{} { return nil }

func writeCall(key string) writer.Call {
	return writer.Call{
		WriteCall: journal.WriteCall{Scope: "test", Kind: "batch", Key: key, Idempotent: true, Hosts: []string{"fm100h1"}, RequestDigest: "d1"},
		Retry:     writer.DefaultRetry,
	}
}

// TestSiteLockGateStopsTheWriter drives the real writer with the gate the
// SiteLock returns: an attempt runs while the lock is held, and once failed
// renewals expire the lock the next write is refused with ErrLockLost.
func TestSiteLockGateStopsTheWriter(t *testing.T) {
	f := clustertest.New("nico-mat")
	f.Bound = cluster.CallTimeout
	fast := Timing{RenewEvery: 20 * time.Millisecond, LeaseDuration: time.Second, MaxRenewAge: 100 * time.Millisecond, ExpirySkew: 10 * time.Millisecond}
	h, err := (&SiteLock{Cluster: f, Name: name, Timing: fast}).Acquire(bg, me("r1"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(h.Close)
	j, err := journal.Create(t.TempDir(), journal.Header{RunID: "r1", Command: "burst"})
	if err != nil {
		t.Fatal(err)
	}
	w := writer.New(j, h.Gate(), neverStop{}, time.Time{})
	t.Cleanup(func() { w.Close(); _ = j.Close() })

	attempts := 0
	send := func(key string) error {
		_, err := writer.Send(context.Background(), w, writeCall(key), func(context.Context) writer.Attempt {
			attempts++
			return writer.Attempt{}
		})
		return err
	}
	is(t, [2]any{send("k1"), attempts}, [2]any{error(nil), 1}, "a write dispatches while the lock is held")

	// Every renewal fails, so the lock expires and the gate closes by itself.
	for range 50 {
		f.FailNext(clustertest.ReplaceLease, down)
	}
	for i := 0; h.Gate().Check() == nil && i < 300; i++ {
		time.Sleep(10 * time.Millisecond)
	}
	is(t, h.Gate().Check(), ErrLost, "the lock expired")

	err = send("k2")
	is(t, [2]any{errors.Is(err, writer.ErrLockLost), attempts}, [2]any{true, 1}, "a write after expiry is refused with ErrLockLost and never dispatches")
}
