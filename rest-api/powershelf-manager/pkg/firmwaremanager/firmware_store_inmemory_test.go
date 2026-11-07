// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"errors"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
)

var testMAC1 = net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, 0x01}
var testMAC2 = net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, 0x02}
var testMAC3 = net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, 0x03}

// --- Get ---

func TestInMemoryStore_GetMiss_ReturnsSqlErrNoRows(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.Get(ctx, testMAC1, powershelf.PMC)

	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrNotFound),
		"Get on empty store must return ErrNotFound, got: %v", err)
}

func TestInMemoryStore_GetMiss_WrongComponent(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	_, err = store.Get(ctx, testMAC1, powershelf.PSU)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrNotFound))
}

func TestInMemoryStore_GetMiss_WrongMAC(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	_, err = store.Get(ctx, testMAC2, powershelf.PMC)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrNotFound))
}

// --- CreateOrReplace ---

func TestInMemoryStore_CreateOrReplace_ThenGet(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	rec, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	assert.Equal(t, testMAC1.String(), rec.PmcMacAddress.String())
	assert.Equal(t, powershelf.PMC, rec.Component)
	assert.Equal(t, "1.0.0", rec.VersionFrom)
	assert.Equal(t, "2.0.0", rec.VersionTo)
	assert.Equal(t, powershelf.FirmwareStateQueued, rec.State)
	assert.Empty(t, rec.ErrorMessage)
	assert.Empty(t, rec.JobID)
	assert.False(t, rec.LastTransitionTime.IsZero())
	assert.False(t, rec.UpdatedAt.IsZero())

	got, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, rec.VersionFrom, got.VersionFrom)
	assert.Equal(t, rec.VersionTo, got.VersionTo)
	assert.Equal(t, rec.State, got.State)
}

func TestInMemoryStore_CreateOrReplace_Upsert(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	rec2, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "2.0.0", "3.0.0")
	require.NoError(t, err)
	assert.Equal(t, "2.0.0", rec2.VersionFrom)
	assert.Equal(t, "3.0.0", rec2.VersionTo)
	assert.Equal(t, powershelf.FirmwareStateQueued, rec2.State)

	got, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, "2.0.0", got.VersionFrom)
	assert.Equal(t, "3.0.0", got.VersionTo)
}

func TestInMemoryStore_CreateOrReplace_ResetsNonQueuedState(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateFailed, "hw error")
	require.NoError(t, err)

	rec, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.True(t, rec.IsTerminal())

	// Upsert should reset to Queued
	replaced, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "2.0.0", "3.0.0")
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateQueued, replaced.State)
	assert.Empty(t, replaced.ErrorMessage)

	got, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateQueued, got.State)
	assert.False(t, got.IsTerminal())
}

func TestInMemoryStore_DifferentComponents_AreIndependent(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	_, err = store.CreateOrReplace(ctx, testMAC1, powershelf.PSU, "3.0.0", "4.0.0")
	require.NoError(t, err)

	pmcRec, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, "1.0.0", pmcRec.VersionFrom)

	psuRec, err := store.Get(ctx, testMAC1, powershelf.PSU)
	require.NoError(t, err)
	assert.Equal(t, "3.0.0", psuRec.VersionFrom)
}

func TestInMemoryStore_DifferentMACs_AreIndependent(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	_, err = store.CreateOrReplace(ctx, testMAC2, powershelf.PMC, "5.0.0", "6.0.0")
	require.NoError(t, err)

	rec1, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, "1.0.0", rec1.VersionFrom)

	rec2, err := store.Get(ctx, testMAC2, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, "5.0.0", rec2.VersionFrom)

	// SetState on one does not affect the other
	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateCompleted, "")
	require.NoError(t, err)

	rec2After, err := store.Get(ctx, testMAC2, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateQueued, rec2After.State)
}

// --- SetState ---

func TestInMemoryStore_SetState_FullLifecycle(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Queued -> Verifying
	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateVerifying, "")
	require.NoError(t, err)

	rec, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, powershelf.FirmwareStateVerifying, rec.State)
	assert.Empty(t, rec.ErrorMessage)
	assert.False(t, rec.IsTerminal())

	// Verifying -> Completed
	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateCompleted, "")
	require.NoError(t, err)

	rec, _ = store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, powershelf.FirmwareStateCompleted, rec.State)
	assert.True(t, rec.IsTerminal())
}

func TestInMemoryStore_SetState_WithError(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateFailed, "connection timeout")
	require.NoError(t, err)

	rec, err := store.Get(ctx, testMAC1, powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateFailed, rec.State)
	assert.Equal(t, "connection timeout", rec.ErrorMessage)
	assert.True(t, rec.IsTerminal())
}

func TestInMemoryStore_SetState_NotFound(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	err := store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateVerifying, "")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "firmware update not found")
}

func TestInMemoryStore_SetState_NoopOnSameStateAndMessage(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateFailed, "err")
	require.NoError(t, err)

	rec1, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	updatedAt1 := rec1.UpdatedAt
	transitionTime1 := rec1.LastTransitionTime

	time.Sleep(5 * time.Millisecond)

	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateFailed, "err")
	require.NoError(t, err)

	rec2, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, updatedAt1, rec2.UpdatedAt, "UpdatedAt should not change on noop")
	assert.Equal(t, transitionTime1, rec2.LastTransitionTime, "LastTransitionTime should not change on noop")
}

func TestInMemoryStore_SetState_SameStateNewMessage_UpdatesMessageAndUpdatedAt(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateVerifying, "attempt 1")
	require.NoError(t, err)

	rec1, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	transitionTime1 := rec1.LastTransitionTime

	time.Sleep(5 * time.Millisecond)

	// Same state, different error message (transient retry in handleOneUpdate)
	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateVerifying, "attempt 2")
	require.NoError(t, err)

	rec2, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, "attempt 2", rec2.ErrorMessage)
	assert.True(t, rec2.UpdatedAt.After(rec1.UpdatedAt),
		"UpdatedAt should advance when error message changes")
	assert.Equal(t, transitionTime1, rec2.LastTransitionTime,
		"LastTransitionTime should NOT change when state stays the same")
}

func TestInMemoryStore_SetState_UpdatesTimestamps(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	created, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	time.Sleep(5 * time.Millisecond)

	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateVerifying, "")
	require.NoError(t, err)

	rec, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.True(t, rec.LastTransitionTime.After(created.LastTransitionTime),
		"LastTransitionTime should advance on state change")
	assert.True(t, rec.UpdatedAt.After(created.UpdatedAt),
		"UpdatedAt should advance on state change")
}

// --- GetAllPending ---

func TestInMemoryStore_GetAllPending(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	_, err = store.CreateOrReplace(ctx, testMAC2, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	_, err = store.CreateOrReplace(ctx, testMAC1, powershelf.PSU, "3.0.0", "4.0.0")
	require.NoError(t, err)

	pending, err := store.GetAllPending(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 3)

	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateCompleted, "")
	require.NoError(t, err)

	err = store.SetState(ctx, testMAC2, powershelf.PMC, powershelf.FirmwareStateFailed, "hw error")
	require.NoError(t, err)

	pending, err = store.GetAllPending(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 1, "Only the PSU update should remain pending")
	assert.Equal(t, powershelf.PSU, pending[0].Component)
}

func TestInMemoryStore_GetAllPending_EmptyStore(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	pending, err := store.GetAllPending(ctx)
	require.NoError(t, err)
	assert.Empty(t, pending)
}

func TestInMemoryStore_GetAllPending_IncludesVerifying(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	err = store.SetState(ctx, testMAC1, powershelf.PMC, powershelf.FirmwareStateVerifying, "")
	require.NoError(t, err)

	pending, err := store.GetAllPending(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 1, "Verifying state should be included in pending")
	assert.Equal(t, powershelf.FirmwareStateVerifying, pending[0].State)
}

func TestInMemoryStore_GetAllPending_CopyOnRead(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	pending, err := store.GetAllPending(ctx)
	require.NoError(t, err)
	require.Len(t, pending, 1)

	// Mutate the returned record
	pending[0].State = powershelf.FirmwareStateCompleted
	pending[0].VersionTo = "9.9.9"

	// Store should be unaffected
	fresh, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, powershelf.FirmwareStateQueued, fresh.State)
	assert.Equal(t, "2.0.0", fresh.VersionTo)
}

// --- Copy safety ---

func TestInMemoryStore_Get_CopyOnRead(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	_, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	got, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	got.State = powershelf.FirmwareStateCompleted
	got.VersionTo = "9.9.9"

	fresh, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, powershelf.FirmwareStateQueued, fresh.State,
		"Mutating returned record must not affect store")
	assert.Equal(t, "2.0.0", fresh.VersionTo,
		"Mutating returned record must not affect store")
}

func TestInMemoryStore_CreateOrReplace_CopyOnReturn(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	created, err := store.CreateOrReplace(ctx, testMAC1, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Mutate the returned record
	created.State = powershelf.FirmwareStateCompleted

	// Store should be unaffected
	got, _ := store.Get(ctx, testMAC1, powershelf.PMC)
	assert.Equal(t, powershelf.FirmwareStateQueued, got.State)
}

// --- Concurrency ---

func TestInMemoryStore_ConcurrentAccess(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	// Pre-populate
	for i := 0; i < 50; i++ {
		mac := net.HardwareAddr{0x00, 0x11, 0x22, 0x33, byte(i >> 8), byte(i)}
		_, err := store.CreateOrReplace(ctx, mac, powershelf.PMC, "1.0.0", "2.0.0")
		require.NoError(t, err)
	}

	var wg sync.WaitGroup
	errs := make(chan error, 500)

	// Concurrent writers: create/replace and set state
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			mac := net.HardwareAddr{0x00, 0x11, 0x22, 0x33, byte(id >> 8), byte(id)}
			for j := 0; j < 20; j++ {
				if _, err := store.CreateOrReplace(ctx, mac, powershelf.PMC, "1.0.0", "2.0.0"); err != nil {
					errs <- err
				}
				if err := store.SetState(ctx, mac, powershelf.PMC, powershelf.FirmwareStateVerifying, ""); err != nil {
					errs <- err
				}
			}
		}(i)
	}

	// Concurrent readers: Get and GetAllPending
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			mac := net.HardwareAddr{0x00, 0x11, 0x22, 0x33, byte(id >> 8), byte(id)}
			for j := 0; j < 20; j++ {
				_, _ = store.Get(ctx, mac, powershelf.PMC)
				_, _ = store.GetAllPending(ctx)
			}
		}(i)
	}

	wg.Wait()
	close(errs)

	for err := range errs {
		t.Errorf("concurrent operation failed: %v", err)
	}
}

// --- Lifecycle ---

func TestInMemoryStore_StartStop_AreNoops(t *testing.T) {
	store := NewInMemoryStore()
	ctx := context.Background()

	assert.NoError(t, store.Start(ctx))
	assert.NoError(t, store.Stop(ctx))
}

// --- recordToDomain ---

func TestRecordToDomain(t *testing.T) {
	rec := &FirmwareUpdateRecord{
		PmcMacAddress: testMAC1,
		Component:     powershelf.PMC,
		VersionFrom:   "1.0.0",
		VersionTo:     "2.0.0",
		State:         powershelf.FirmwareStateVerifying,
		JobID:         "job-123",
		ErrorMessage:  "transient",
	}

	domain := recordToDomain(rec)
	require.NotNil(t, domain)
	assert.Equal(t, testMAC1.String(), domain.PmcMacAddress)
	assert.Equal(t, powershelf.PMC, domain.Component)
	assert.Equal(t, "1.0.0", domain.VersionFrom)
	assert.Equal(t, "2.0.0", domain.VersionTo)
	assert.Equal(t, powershelf.FirmwareStateVerifying, domain.State)
	assert.Equal(t, "job-123", domain.JobID)
	assert.Equal(t, "transient", domain.ErrorMessage)
}

func TestRecordToDomain_Nil(t *testing.T) {
	assert.Nil(t, recordToDomain(nil))
}

// --- IsTerminal ---

func TestFirmwareUpdateRecord_IsTerminal(t *testing.T) {
	tests := []struct {
		state    powershelf.FirmwareState
		terminal bool
	}{
		{powershelf.FirmwareStateQueued, false},
		{powershelf.FirmwareStateVerifying, false},
		{powershelf.FirmwareStateCompleted, true},
		{powershelf.FirmwareStateFailed, true},
	}

	for _, tc := range tests {
		t.Run(string(tc.state), func(t *testing.T) {
			rec := &FirmwareUpdateRecord{State: tc.state}
			assert.Equal(t, tc.terminal, rec.IsTerminal())
		})
	}
}
