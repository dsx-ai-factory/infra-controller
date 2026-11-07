// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sourcelist

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/controller"
)

// fakeClock drives a Registry deterministically: the registry has no timers,
// so time only moves when a test advances it between observations.
type fakeClock struct {
	now time.Time
}

func newFakeClock() *fakeClock {
	return &fakeClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
}

func (c *fakeClock) Now() time.Time { return c.now }

func (c *fakeClock) Advance(d time.Duration) { c.now = c.now.Add(d) }

func newTestRegistry(t *testing.T, debounce time.Duration) (*Registry, *fakeClock) {
	t.Helper()
	clock := newFakeClock()
	r := NewRegistry(WithDebounce(debounce))
	r.now = clock.Now
	return r, clock
}

// The controller's default cadence: discovery runs every 30s and the debounce
// window is 5s, so a change is confirmed by the next pass that still sees it.
const (
	discoveryInterval = 30 * time.Second
	debounceWindow    = 5 * time.Second
)

var (
	srcA  = Source{Name: "mat-a", BaseURL: "https://mat-a.ns.svc.cluster.local:8443", Pod: "mat-a-pod"}
	srcB  = Source{Name: "mat-b", BaseURL: "https://mat-b.ns.svc.cluster.local:8443", Pod: "mat-b-pod"}
	srcC  = Source{Name: "mat-c", BaseURL: "https://mat-c.ns.svc.cluster.local:8443", Pod: "mat-c-pod"}
	srcA2 = Source{Name: "mat-a", BaseURL: "https://mat-a.ns.svc.cluster.local:9443", Pod: "mat-a-pod"}
)

func TestRegistry_InitialState(t *testing.T) {
	r, _ := newTestRegistry(t, DefaultDebounce)

	snap := r.Snapshot()
	assert.False(t, snap.Ready)
	assert.Equal(t, uint64(0), snap.Generation)
	require.NotNil(t, snap.Sources, "sources must encode as an array, not null")
	assert.Empty(t, snap.Sources)
}

func TestRegistry_FirstObservationPublishesImmediately(t *testing.T) {
	r, _ := newTestRegistry(t, DefaultDebounce)

	r.Observe([]Source{srcB, srcA})

	snap := r.Snapshot()
	assert.True(t, snap.Ready)
	assert.Equal(t, uint64(1), snap.Generation)
	assert.Equal(t, []Source{srcA, srcB}, snap.Sources, "sources are sorted by name")
}

func TestRegistry_FirstObservationEmptyIsReady(t *testing.T) {
	r, _ := newTestRegistry(t, DefaultDebounce)

	r.Observe(nil)

	snap := r.Snapshot()
	assert.True(t, snap.Ready)
	assert.Equal(t, uint64(1), snap.Generation)
	require.NotNil(t, snap.Sources)
	assert.Empty(t, snap.Sources)
}

func TestRegistry_SameSetKeepsGeneration(t *testing.T) {
	r, clock := newTestRegistry(t, DefaultDebounce)
	r.Observe([]Source{srcA, srcB})

	for i := 0; i < 10; i++ {
		clock.Advance(discoveryInterval)
		// Order differs each time; the set is the same.
		if i%2 == 0 {
			r.Observe([]Source{srcB, srcA})
		} else {
			r.Observe([]Source{srcA, srcB})
		}
	}

	snap := r.Snapshot()
	assert.Equal(t, uint64(1), snap.Generation)
	assert.Equal(t, []Source{srcA, srcB}, snap.Sources)
}

func TestRegistry_PodChangeDoesNotBumpGeneration(t *testing.T) {
	r, clock := newTestRegistry(t, DefaultDebounce)
	r.Observe([]Source{srcA})

	restarted := srcA
	restarted.Pod = "mat-a-pod-restarted"
	clock.Advance(discoveryInterval)
	r.Observe([]Source{restarted})

	snap := r.Snapshot()
	assert.Equal(t, uint64(1), snap.Generation)
	assert.Equal(t, []Source{restarted}, snap.Sources, "pod name is refreshed in place")
}

// observation is one discovery pass in a debounce scenario: the clock moves
// forward by advance, the registry observes sources, and the published
// generation must equal wantGeneration afterwards. A non-nil wantSources also
// checks the published set (a snapshot never carries a nil slice).
type observation struct {
	advance        time.Duration
	sources        []Source
	wantGeneration uint64
	wantSources    []Source
}

func TestRegistry_Debounce(t *testing.T) {
	tests := []struct {
		name     string
		debounce time.Duration
		passes   []observation
	}{
		{
			// The first observation publishes at once. A change seen on the
			// next pass stays pending, invisible to consumers, until the pass
			// after it reports the same set beyond the window.
			name:     "change is published by the next pass after the window",
			debounce: debounceWindow,
			passes: []observation{
				{sources: []Source{srcA}, wantGeneration: 1},
				{advance: discoveryInterval, sources: []Source{srcA, srcB}, wantGeneration: 1, wantSources: []Source{srcA}},
				{advance: discoveryInterval, sources: []Source{srcA, srcB}, wantGeneration: 2, wantSources: []Source{srcA, srcB}},
			},
		},
		{
			// Discovery running more often than the window: the change is
			// confirmed only by the first observation at least debounce after
			// the first sighting.
			name:     "confirmation inside the window waits",
			debounce: debounceWindow,
			passes: []observation{
				{sources: []Source{srcA}, wantGeneration: 1},
				{advance: time.Second, sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: 2 * time.Second, sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: 3 * time.Second, sources: []Source{srcA, srcB}, wantGeneration: 2, wantSources: []Source{srcA, srcB}},
			},
		},
		{
			// srcB is missing from one pass and back on the next, with the
			// window shorter than the interval between passes. The set stays
			// stable afterwards: still nothing to publish.
			name:     "flap across one pass is suppressed",
			debounce: debounceWindow,
			passes: []observation{
				{sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: discoveryInterval, sources: []Source{srcA}, wantGeneration: 1, wantSources: []Source{srcA, srcB}},
				{advance: discoveryInterval, sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: discoveryInterval, sources: []Source{srcA, srcB}, wantGeneration: 1, wantSources: []Source{srcA, srcB}},
			},
		},
		{
			// Discovery running more often than the window: srcB disappears
			// and comes back within it.
			name:     "flap within the window is suppressed",
			debounce: debounceWindow,
			passes: []observation{
				{sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: time.Second, sources: []Source{srcA}, wantGeneration: 1},
				{advance: time.Second, sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: time.Hour, sources: []Source{srcA, srcB}, wantGeneration: 1, wantSources: []Source{srcA, srcB}},
			},
		},
		{
			// The fourth pass is 5s after the first change but only 2s after
			// the second one, which restarted the window.
			name:     "a different pending set restarts the window",
			debounce: debounceWindow,
			passes: []observation{
				{sources: []Source{srcA}, wantGeneration: 1},
				{advance: time.Second, sources: []Source{srcA, srcB}, wantGeneration: 1},
				{advance: 3 * time.Second, sources: []Source{srcA, srcB, srcC}, wantGeneration: 1},
				{advance: 2 * time.Second, sources: []Source{srcA, srcB, srcC}, wantGeneration: 1, wantSources: []Source{srcA}},
				{advance: 3 * time.Second, sources: []Source{srcA, srcB, srcC}, wantGeneration: 2, wantSources: []Source{srcA, srcB, srcC}},
			},
		},
		{
			name:     "zero debounce publishes with the pass that sees the change",
			debounce: 0,
			passes: []observation{
				{sources: []Source{srcA}, wantGeneration: 1},
				{advance: discoveryInterval, sources: []Source{srcA, srcB}, wantGeneration: 2, wantSources: []Source{srcA, srcB}},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r, clock := newTestRegistry(t, tt.debounce)
			for i, pass := range tt.passes {
				clock.Advance(pass.advance)
				r.Observe(pass.sources)
				snap := r.Snapshot()
				assert.Equal(t, pass.wantGeneration, snap.Generation, "generation after pass %d", i+1)
				if pass.wantSources != nil {
					assert.Equal(t, pass.wantSources, snap.Sources, "sources after pass %d", i+1)
				}
			}
		})
	}
}

func TestRegistry_GenerationIncrementsPerKindOfChange(t *testing.T) {
	r, clock := newTestRegistry(t, 0)
	r.Observe([]Source{srcA})
	assert.Equal(t, uint64(1), r.Snapshot().Generation)

	// Add.
	clock.Advance(discoveryInterval)
	r.Observe([]Source{srcA, srcB})
	assert.Equal(t, uint64(2), r.Snapshot().Generation)

	// URL change for an existing name.
	clock.Advance(discoveryInterval)
	r.Observe([]Source{srcA2, srcB})
	snap := r.Snapshot()
	assert.Equal(t, uint64(3), snap.Generation)
	assert.Equal(t, []Source{srcA2, srcB}, snap.Sources)

	// Remove.
	clock.Advance(discoveryInterval)
	r.Observe([]Source{srcB})
	snap = r.Snapshot()
	assert.Equal(t, uint64(4), snap.Generation)
	assert.Equal(t, []Source{srcB}, snap.Sources)

	// Remove everything.
	clock.Advance(discoveryInterval)
	r.Observe(nil)
	snap = r.Snapshot()
	assert.Equal(t, uint64(5), snap.Generation)
	assert.True(t, snap.Ready)
	require.NotNil(t, snap.Sources)
	assert.Empty(t, snap.Sources)
}

func TestRegistry_SnapshotIsACopy(t *testing.T) {
	r, _ := newTestRegistry(t, DefaultDebounce)
	r.Observe([]Source{srcA, srcB})

	snap := r.Snapshot()
	snap.Sources[0].Name = "mutated"

	assert.Equal(t, "mat-a", r.Snapshot().Sources[0].Name)
}

func TestRegistry_RealClockConfirmsAfterTheWindow(t *testing.T) {
	// Exercise the real clock once with a short window.
	r := NewRegistry(WithDebounce(20 * time.Millisecond))
	r.Observe([]Source{srcA})
	r.Observe([]Source{srcA, srcB})
	assert.Equal(t, uint64(1), r.Snapshot().Generation)

	time.Sleep(30 * time.Millisecond)
	assert.Equal(t, uint64(1), r.Snapshot().Generation, "the clock alone publishes nothing")

	r.Observe([]Source{srcA, srcB})
	assert.Equal(t, uint64(2), r.Snapshot().Generation)
	assert.Equal(t, []Source{srcA, srcB}, r.Snapshot().Sources)
}

// stubDiscovery returns canned results for WrapDiscovery tests.
type stubDiscovery struct {
	instances []controller.DiscoveredInstance
	err       error
	calls     int
}

func (s *stubDiscovery) Discover(context.Context) ([]controller.DiscoveredInstance, error) {
	s.calls++
	return s.instances, s.err
}

func TestFromDiscovered(t *testing.T) {
	got := fromDiscovered([]controller.DiscoveredInstance{
		{URL: "https://svc.ns.svc.cluster.local:8443", PodName: "mat-0", ServiceName: "svc"},
	})
	assert.Equal(t, []Source{{Name: "svc", BaseURL: "https://svc.ns.svc.cluster.local:8443", Pod: "mat-0"}}, got)

	assert.NotNil(t, fromDiscovered(nil))
	assert.Empty(t, fromDiscovered(nil))
}

func TestWrapDiscovery_RecordsSuccess(t *testing.T) {
	r, _ := newTestRegistry(t, DefaultDebounce)
	stub := &stubDiscovery{instances: []controller.DiscoveredInstance{
		{URL: srcB.BaseURL, PodName: srcB.Pod, ServiceName: srcB.Name},
		{URL: srcA.BaseURL, PodName: srcA.Pod, ServiceName: srcA.Name},
	}}

	wrapped := WrapDiscovery(stub, r)
	got, err := wrapped.Discover(context.Background())
	require.NoError(t, err)
	assert.Equal(t, stub.instances, got, "results are passed through unchanged")
	assert.Equal(t, 1, stub.calls)

	snap := r.Snapshot()
	assert.True(t, snap.Ready)
	assert.Equal(t, []Source{srcA, srcB}, snap.Sources)
}

func TestWrapDiscovery_ErrorLeavesRegistryUntouched(t *testing.T) {
	r, _ := newTestRegistry(t, DefaultDebounce)
	stub := &stubDiscovery{err: errors.New("boom")}

	_, err := WrapDiscovery(stub, r).Discover(context.Background())
	require.Error(t, err)

	snap := r.Snapshot()
	assert.False(t, snap.Ready, "a failed discovery must not mark the registry ready")
	assert.Equal(t, uint64(0), snap.Generation)
}
