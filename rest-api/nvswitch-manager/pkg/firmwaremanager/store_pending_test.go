// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestInMemoryUpdateStore_GetPendingUpdates(t *testing.T) {
	ctx := t.Context()
	store := NewInMemoryUpdateStore()
	createdAt := time.Date(2026, time.January, 1, 0, 0, 0, 0, time.UTC)
	oldestActiveID, newestActiveID := uuid.New(), uuid.New()
	completedID, failedID, missingID := uuid.New(), uuid.New(), uuid.New()
	firstQueuedID, secondQueuedID := uuid.New(), uuid.New()

	// Save active records before ready queues so queue priority must override
	// age. Their sequence orders oppose their save order.
	updates := []FirmwareUpdate{
		{ID: oldestActiveID, State: StateVerify, SequenceOrder: 9},
		{ID: newestActiveID, State: StateInstall, SequenceOrder: 1},
		{ID: completedID, State: StateCompleted},
		{ID: failedID, State: StateFailed},
		{ID: uuid.New(), State: StateCancelled},
		{ID: secondQueuedID, State: StateQueued, SequenceOrder: 5, PredecessorID: &completedID},
		{ID: firstQueuedID, State: StateQueued, SequenceOrder: 4},
		// Blocked queues sort ahead of ready queues if mistakenly admitted.
		{ID: uuid.New(), State: StateQueued, SequenceOrder: 1, PredecessorID: &missingID},
		{ID: uuid.New(), State: StateQueued, SequenceOrder: 2, PredecessorID: &failedID},
		{ID: uuid.New(), State: StateQueued, SequenceOrder: 3, PredecessorID: &oldestActiveID},
	}
	for i := range updates {
		updates[i].CreatedAt = createdAt
		require.NoError(t, store.Save(ctx, &updates[i]))
	}

	// Save overwrites UpdatedAt, so set the stored fixture timestamps explicitly.
	store.mu.Lock()
	store.updates[oldestActiveID].UpdatedAt = createdAt.Add(time.Minute)
	store.updates[newestActiveID].UpdatedAt = createdAt.Add(2 * time.Minute)
	store.mu.Unlock()

	oldestActive, err := store.Get(ctx, oldestActiveID)
	require.NoError(t, err)
	newestActive, err := store.Get(ctx, newestActiveID)
	require.NoError(t, err)
	require.True(t, oldestActive.UpdatedAt.Before(newestActive.UpdatedAt),
		"active fixture records must have distinct, increasing saved timestamps")

	tests := []struct {
		name    string
		limit   int
		wantIDs []uuid.UUID
	}{
		{name: "zero limit", limit: 0, wantIDs: []uuid.UUID{}},
		{name: "truncate queued tier", limit: 1, wantIDs: []uuid.UUID{firstQueuedID}},
		{name: "truncate active tier", limit: 3, wantIDs: []uuid.UUID{firstQueuedID, secondQueuedID, oldestActiveID}},
		{
			name:    "eligible queues then active records only",
			limit:   len(updates),
			wantIDs: []uuid.UUID{firstQueuedID, secondQueuedID, oldestActiveID, newestActiveID},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pending, err := store.GetPendingUpdates(ctx, tt.limit)
			require.NoError(t, err)
			gotIDs := make([]uuid.UUID, 0, len(pending))
			for _, update := range pending {
				require.NotNil(t, update)
				gotIDs = append(gotIDs, update.ID)
			}
			assert.Equal(t, tt.wantIDs, gotIDs)
		})
	}
}
