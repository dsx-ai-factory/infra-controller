// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestInMemoryUpdateStore_CancelRemainingInBundle(t *testing.T) {
	bundleID := uuid.New()
	otherBundleID := uuid.New()
	switchID := uuid.New()
	const afterSequence = 2

	// Each excluded record isolates one boundary while the two eligible records
	// prove that cancellation counts and updates every later queued record.
	records := []struct {
		name          string
		bundleID      *uuid.UUID
		sequence      int
		state         UpdateState
		wantCancelled bool
	}{
		{name: "earlier queued", bundleID: &bundleID, sequence: 1, state: StateQueued},
		{name: "equal queued", bundleID: &bundleID, sequence: 2, state: StateQueued},
		{name: "next queued", bundleID: &bundleID, sequence: 3, state: StateQueued, wantCancelled: true},
		{name: "later queued", bundleID: &bundleID, sequence: 5, state: StateQueued, wantCancelled: true},
		{name: "active", bundleID: &bundleID, sequence: 3, state: StateInstall},
		{name: "completed", bundleID: &bundleID, sequence: 3, state: StateCompleted},
		{name: "different bundle", bundleID: &otherBundleID, sequence: 3, state: StateQueued},
		{name: "standalone", sequence: 3, state: StateQueued},
	}
	ctx := t.Context()
	store := NewInMemoryUpdateStore()
	before := make([]FirmwareUpdate, len(records))
	for i, record := range records {
		update := NewFirmwareUpdate(switchID, nvswitch.CPLD, "test-bundle", StrategySSH, "test-version")
		update.BundleUpdateID = record.bundleID
		update.SequenceOrder = record.sequence
		update.State = record.state
		update.ErrorMessage = "existing diagnostic: " + record.name
		err := store.Save(ctx, update)
		require.NoError(t, err)
		saved, err := store.Get(ctx, update.ID)
		require.NoError(t, err)
		require.NotNil(t, saved)
		before[i] = *saved
	}

	started := time.Now()
	count, err := store.CancelRemainingInBundle(ctx, bundleID, afterSequence, nvswitch.BMC)
	finished := time.Now()
	require.NoError(t, err)
	assert.Equal(t, 2, count)

	after := make([]FirmwareUpdate, len(records))
	for i, record := range records {
		t.Run(record.name, func(t *testing.T) {
			got, err := store.Get(ctx, before[i].ID)
			require.NoError(t, err)
			require.NotNil(t, got)
			want := before[i]
			if record.wantCancelled {
				want.State = StateCancelled
				want.ErrorMessage = "cancelled due to BMC failure"
				assert.False(t, got.UpdatedAt.Before(started), "cancellation timestamp precedes the call")
				assert.False(t, got.UpdatedAt.After(finished), "cancellation timestamp follows the call")
				want.UpdatedAt = got.UpdatedAt
			}
			assert.Equal(t, want, *got)
			after[i] = *got
		})
	}

	// Repeating the request must neither recount nor rewrite any record,
	// including the cancellation diagnostic and timestamp from the first call.
	count, err = store.CancelRemainingInBundle(ctx, bundleID, afterSequence, nvswitch.BMC)
	require.NoError(t, err)
	assert.Zero(t, count)
	for i, record := range records {
		got, err := store.Get(ctx, after[i].ID)
		require.NoError(t, err, record.name)
		require.NotNil(t, got, record.name)
		assert.Equal(t, after[i], *got, record.name)
	}
}
