// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// fakeReader is an in-memory StatusReader for tests. It is goroutine-safe
// so a test can mutate state from another goroutine while the gate polls.
type fakeReader struct {
	*MemReader
	mu       sync.Mutex
	statuses map[string]*types.ComponentOperationStatus
	hosts    map[string][]string
	calls    atomic.Int32
}

func newFakeReader() *fakeReader {
	return &fakeReader{
		MemReader: NewMemReader(),
		statuses:  map[string]*types.ComponentOperationStatus{},
		hosts:     map[string][]string{},
	}
}

func (f *fakeReader) setStatus(id string, s *types.ComponentOperationStatus) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.statuses[id] = s
}

func (f *fakeReader) setHosts(rackID string, hosts []string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.hosts[rackID] = hosts
}

func (f *fakeReader) GetStatusesByExternalIDs(_ context.Context, ids []string) (map[string]*types.ComponentOperationStatus, error) {
	f.calls.Add(1)
	f.mu.Lock()
	defer f.mu.Unlock()
	out := make(map[string]*types.ComponentOperationStatus, len(ids))
	for _, id := range ids {
		if s, ok := f.statuses[id]; ok {
			out[id] = s
		}
	}
	return out, nil
}

func (f *fakeReader) GetHostExternalIDsByRackIDs(_ context.Context, rackIDs []string) (map[string][]string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	out := make(map[string][]string, len(rackIDs))
	for _, id := range rackIDs {
		if h, ok := f.hosts[id]; ok {
			out[id] = h
		}
	}
	return out, nil
}

func readyStatus() *types.ComponentOperationStatus {
	return &types.ComponentOperationStatus{Phase: types.PhaseReady}
}

func inUseStatus() *types.ComponentOperationStatus {
	return &types.ComponentOperationStatus{
		Phase:             types.PhaseInUse,
		Reason:            "Assigned/Provisioning",
		BlockedOperations: []types.OperationType{types.OperationTypePowerControl, types.OperationTypeFirmwareControl},
	}
}

func TestWaitForComponentsReady_EmptyInputShortCircuits(t *testing.T) {
	g := NewDBGate(newFakeReader(), time.Second, 10*time.Millisecond)
	require.NoError(t, g.WaitForComponentsReady(context.Background(), nil, types.OperationTypePowerControl))
	require.NoError(t, g.WaitForComponentsReady(context.Background(), []string{}, types.OperationTypePowerControl))
}

func TestWaitForComponentsReady_NilGateShortCircuits(t *testing.T) {
	var g *DBGate
	require.NoError(t, g.WaitForComponentsReady(context.Background(), []string{"m1"}, types.OperationTypePowerControl))
}

func TestWaitForComponentsReady_AllReady(t *testing.T) {
	r := newFakeReader()
	r.setStatus("m1", readyStatus())
	r.setStatus("m2", readyStatus())

	g := NewDBGate(r, time.Second, 10*time.Millisecond)
	require.NoError(t, g.WaitForComponentsReady(context.Background(), []string{"m1", "m2"}, types.OperationTypePowerControl))
	require.Equal(t, int32(1), r.calls.Load(), "ready on first poll should not retry")
}

func TestWaitForComponentsReady_MissingStatusIsPermissive(t *testing.T) {
	r := newFakeReader()
	g := NewDBGate(r, 50*time.Millisecond, 10*time.Millisecond)
	require.NoError(t, g.WaitForComponentsReady(context.Background(), []string{"m1"}, types.OperationTypePowerControl))
}

func TestWaitForComponentsReady_TimesOutWhileBlocking(t *testing.T) {
	r := newFakeReader()
	r.setStatus("m1", inUseStatus())

	g := NewDBGate(r, 50*time.Millisecond, 10*time.Millisecond)
	err := g.WaitForComponentsReady(context.Background(), []string{"m1"}, types.OperationTypePowerControl)
	require.Error(t, err)
	require.Contains(t, err.Error(), "m1")
}

func TestWaitForComponentsReady_TransitionsFromBlockingToReady(t *testing.T) {
	r := newFakeReader()
	r.setStatus("m1", inUseStatus())

	g := NewDBGate(r, time.Second, 10*time.Millisecond)

	go func() {
		time.Sleep(25 * time.Millisecond)
		r.setStatus("m1", readyStatus())
	}()

	require.NoError(t, g.WaitForComponentsReady(context.Background(), []string{"m1"}, types.OperationTypePowerControl))
	require.GreaterOrEqual(t, r.calls.Load(), int32(2), "should have polled more than once")
}

func TestWaitForComponentsReady_PartialBlocking(t *testing.T) {
	r := newFakeReader()
	r.setStatus("alpha", readyStatus())
	r.setStatus("beta", inUseStatus())

	g := NewDBGate(r, 30*time.Millisecond, 10*time.Millisecond)
	err := g.WaitForComponentsReady(context.Background(), []string{"alpha", "beta"}, types.OperationTypePowerControl)
	require.Error(t, err)
	require.Contains(t, err.Error(), "beta")
	require.NotContains(t, err.Error(), "alpha")
}

func TestWaitForComponentsReady_OperationScopedBlock(t *testing.T) {
	r := newFakeReader()
	// Only firmware is blocked; power control should proceed.
	r.setStatus("m1", &types.ComponentOperationStatus{
		Phase:             types.PhaseReady,
		BlockedOperations: []types.OperationType{types.OperationTypeFirmwareControl},
	})

	g := NewDBGate(r, 50*time.Millisecond, 10*time.Millisecond)
	require.NoError(t, g.WaitForComponentsReady(context.Background(), []string{"m1"}, types.OperationTypePowerControl))

	err := g.WaitForComponentsReady(context.Background(), []string{"m1"}, types.OperationTypeFirmwareControl)
	require.Error(t, err)
}

func TestWaitForComponentsReady_ContextCancellationStopsPolling(t *testing.T) {
	r := newFakeReader()
	r.setStatus("m1", inUseStatus())

	g := NewDBGate(r, 10*time.Second, 50*time.Millisecond)
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(20 * time.Millisecond)
		cancel()
	}()
	err := g.WaitForComponentsReady(ctx, []string{"m1"}, types.OperationTypePowerControl)
	require.Error(t, err)
	require.True(t, errors.Is(err, context.Canceled))
}

func TestWaitForComponentsReady_DedupsAndIgnoresEmpty(t *testing.T) {
	r := newFakeReader()
	r.setStatus("m1", readyStatus())

	g := NewDBGate(r, time.Second, 10*time.Millisecond)
	require.NoError(t, g.WaitForComponentsReady(
		context.Background(),
		[]string{"", "m1", "m1", ""},
		types.OperationTypePowerControl,
	))
}

func TestWaitForRackHostsReady_NoHosts_Skips(t *testing.T) {
	r := newFakeReader()
	g := NewDBGate(r, time.Second, 10*time.Millisecond)
	require.NoError(t, g.WaitForRackHostsReady(context.Background(), []string{"rack-1"}, types.OperationTypePowerControl))
	require.Equal(t, int32(0), r.calls.Load(), "no hosts => no status reads")
}

func TestWaitForRackHostsReady_ResolvesAndDelegates(t *testing.T) {
	r := newFakeReader()
	r.setHosts("rack-1", []string{"host-1", "host-2"})
	r.setStatus("host-1", readyStatus())
	r.setStatus("host-2", inUseStatus())

	g := NewDBGate(r, 30*time.Millisecond, 10*time.Millisecond)
	err := g.WaitForRackHostsReady(context.Background(), []string{"rack-1"}, types.OperationTypePowerControl)
	require.Error(t, err)
	require.Contains(t, err.Error(), "host-2")
}
