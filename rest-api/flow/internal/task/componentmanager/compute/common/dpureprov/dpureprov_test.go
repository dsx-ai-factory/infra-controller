// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpureprov

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
)

const (
	testHost   = "host-1"
	testDPUA   = "dpu-1a"
	testDPUB   = "dpu-1b"
	testInst   = "inst-tenant-1"
	pollMicro  = 1 * time.Microsecond
	pollMicroT = 5 * time.Microsecond
)

// virtualClock provides a deterministic Now/Sleep pair for tests so the
// poll loop can advance through multiple iterations without real-time
// waits. Sleep advances `now` and decrements the configured pending
// counter so we can simulate "DPU disappears from pending list after N
// polls".
type virtualClock struct {
	now             time.Time
	pollsLeftToDone int
	client          nicoapi.Client
	hostMachineID   string
}

func (v *virtualClock) Now() time.Time { return v.now }

func (v *virtualClock) Sleep(_ context.Context, d time.Duration) error {
	v.now = v.now.Add(d)
	if v.pollsLeftToDone > 0 {
		v.pollsLeftToDone--
		if v.pollsLeftToDone == 0 {
			v.client.SetDpuReprovisioningPending(v.hostMachineID, false)
		}
	}
	return nil
}

// TestReprovisionHostDpus_HappyPath_AssignedHost covers the canonical
// flow: an Assigned host with both DPUs and an attached instance must
// route through InvokeInstancePower(apply_updates=true), see the
// HostUpdateInProgress override inserted *before* the trigger, and have
// it removed after the poll loop sees the host disappear from the
// pending list.
func TestReprovisionHostDpus_HappyPath_AssignedHost(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds(testHost, []string{testDPUA, testDPUB})
	mock.SetHostInstanceID(testHost, testInst)

	clock := &virtualClock{
		now:             time.Unix(0, 0),
		pollsLeftToDone: 2,
		client:          mock,
		hostMachineID:   testHost,
	}

	err := ReprovisionHostDpus(
		context.Background(),
		mock, testHost, true,
		Options{
			PollInterval: pollMicro,
			PollTimeout:  10 * time.Second,
			Now:          clock.Now,
			Sleep:        clock.Sleep,
		},
	)
	require.NoError(t, err)

	// HostUpdateInProgress override was active during the run AND is
	// removed by the time we return.
	assert.Empty(t, mock.HostUpdateOverridesActive(),
		"override must be removed via deferred cleanup")

	// TriggerDpuReprovisioning saw exactly one call against the host
	// machine id (Core fans out to per-DPU server-side).
	triggers := mock.DpuReprovisioningTriggers()
	require.Len(t, triggers, 1)
	assert.Equal(t, testHost, triggers[0].MachineID)
	assert.True(t, triggers[0].UpdateFirmware,
		"update_firmware must be propagated to Core")

	// Assigned host -> InvokeInstancePower with apply_updates=true.
	powerCalls := mock.InstancePowerCalls()
	require.Len(t, powerCalls, 1)
	assert.Equal(t, testInst, powerCalls[0].InstanceID)
	assert.True(t, powerCalls[0].ApplyUpdates)
}

// TestReprovisionHostDpus_HappyPath_UnassignedHost covers the alternate
// power path: a host with DPUs but no attached instance should NOT call
// InvokeInstancePower (which would fail with empty instance id) and
// must fall back to AdminPowerControl. We don't have a per-call mock
// recorder for AdminPowerControl, so this test verifies (a) no instance
// power call happens and (b) the rest of the SAGA still completes.
func TestReprovisionHostDpus_HappyPath_UnassignedHost(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds(testHost, []string{testDPUA})
	mock.SetHostInstanceID(testHost, "")

	clock := &virtualClock{
		now:             time.Unix(0, 0),
		pollsLeftToDone: 1,
		client:          mock,
		hostMachineID:   testHost,
	}

	err := ReprovisionHostDpus(
		context.Background(),
		mock, testHost, true,
		Options{
			PollInterval: pollMicro,
			PollTimeout:  10 * time.Second,
			Now:          clock.Now,
			Sleep:        clock.Sleep,
		},
	)
	require.NoError(t, err)

	assert.Empty(t, mock.InstancePowerCalls(),
		"unassigned host must NOT route the reboot through InvokeInstancePower")
	assert.Len(t, mock.DpuReprovisioningTriggers(), 1)
	assert.Empty(t, mock.HostUpdateOverridesActive())
}

// TestReprovisionHostDpus_NoDpus rejects the request rather than
// silently no-oping. The reconciler skips DPU-less hosts, but here the
// caller (REST request) explicitly opted in, so "no DPUs" is operator
// intent vs. inventory mismatch and must surface.
func TestReprovisionHostDpus_NoDpus(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds(testHost, nil)

	err := ReprovisionHostDpus(
		context.Background(),
		mock, testHost, true,
		Options{},
	)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "no associated DPUs")

	// No mutating calls happen: the early guard runs before any
	// override / trigger / state-machine handoff attempt.
	assert.Empty(t, mock.HostUpdateOverridesActive())
	assert.Empty(t, mock.DpuReprovisioningTriggers())
	assert.Empty(t, mock.InstancePowerCalls())
}

// TestReprovisionHostDpus_TriggerFails_RemovesOverride pins the SAGA's
// compensation contract: a failure between the override insert and the
// final cleanup must still remove the override before returning, so a
// stale "PreventAllocations" alert never blocks the next reconciler
// run.
func TestReprovisionHostDpus_TriggerFails_RemovesOverride(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds(testHost, []string{testDPUA})
	mock.SetHostInstanceID(testHost, testInst)
	mock.SetTriggerDpuReprovisioningError(errors.New("core: precondition failed"))

	err := ReprovisionHostDpus(
		context.Background(),
		mock, testHost, true,
		Options{
			PollInterval: pollMicro,
			PollTimeout:  10 * time.Second,
		},
	)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to trigger DPU reprovisioning")
	assert.Empty(t, mock.HostUpdateOverridesActive(),
		"override must be removed even when the trigger step fails")
}

// TestReprovisionHostDpus_InsertOverrideFails covers the inverse of the
// trigger-fails case: when step 1 fails, the override is by definition
// not in place, so we must NOT issue a remove RPC for an entry we
// never created. The mock removes idempotently so this test mostly
// guards against accidentally swapping the order of steps 1 and 2.
func TestReprovisionHostDpus_InsertOverrideFails(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds(testHost, []string{testDPUA})
	mock.SetInsertHostUpdateOverrideError(errors.New("core: rpc error"))

	err := ReprovisionHostDpus(
		context.Background(),
		mock, testHost, true,
		Options{},
	)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to insert HostUpdateInProgress override")

	// Trigger and instance power must NOT have run.
	assert.Empty(t, mock.DpuReprovisioningTriggers())
	assert.Empty(t, mock.InstancePowerCalls())
}

// TestReprovisionHostDpus_PollTimeout exercises the poll deadline. We
// keep pollsLeftToDone > poll iterations so the host stays "pending"
// forever within the loop's perspective, then assert that the function
// returns a timeout error and still removes the override.
func TestReprovisionHostDpus_PollTimeout(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds(testHost, []string{testDPUA})
	mock.SetHostInstanceID(testHost, testInst)
	// Seed a pending state and never clear it.
	mock.SetDpuReprovisioningPending(testHost, true)

	clock := &virtualClock{
		now:             time.Unix(0, 0),
		pollsLeftToDone: 999, // stays pending past the deadline
		client:          mock,
		hostMachineID:   testHost,
	}

	err := ReprovisionHostDpus(
		context.Background(),
		mock, testHost, true,
		Options{
			PollInterval: 10 * time.Second,
			PollTimeout:  20 * time.Second,
			Now:          clock.Now,
			Sleep:        clock.Sleep,
		},
	)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "timed out")
	assert.Empty(t, mock.HostUpdateOverridesActive(),
		"override must be removed even on poll timeout")
}

// TestReprovisionHosts_MultipleHosts_SerialAndAggregated documents the
// per-host serial execution + accumulated-error contract: a failure on
// host A does not abort host B's run, and the returned error mentions
// every failed host.
func TestReprovisionHosts_MultipleHosts_SerialAndAggregated(t *testing.T) {
	mock := nicoapi.NewMockClient()
	mock.SetHostDpuMachineIds("good", []string{testDPUA})
	mock.SetHostInstanceID("good", testInst)
	mock.SetHostDpuMachineIds("bad", nil) // triggers "no DPUs" failure

	clock := &virtualClock{
		now:             time.Unix(0, 0),
		pollsLeftToDone: 1,
		client:          mock,
		hostMachineID:   "good",
	}

	err := ReprovisionHosts(
		context.Background(),
		mock, []string{"good", "bad"}, true,
		Options{
			PollInterval: pollMicro,
			PollTimeout:  10 * time.Second,
			Now:          clock.Now,
			Sleep:        clock.Sleep,
		},
	)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "1/2 host(s)")
	assert.Contains(t, err.Error(), "bad: ")

	// "good" still went through despite "bad"'s early failure.
	require.Len(t, mock.DpuReprovisioningTriggers(), 1)
	assert.Equal(t, "good", mock.DpuReprovisioningTriggers()[0].MachineID)
}

// TestReprovisionHostDpus_EmptyHostID guards the explicit-input-required
// contract at the entry point so callers get a clean error before any
// RPC is issued.
func TestReprovisionHostDpus_EmptyHostID(t *testing.T) {
	mock := nicoapi.NewMockClient()

	err := ReprovisionHostDpus(
		context.Background(),
		mock, "", true,
		Options{},
	)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "host machine id is required")
}
