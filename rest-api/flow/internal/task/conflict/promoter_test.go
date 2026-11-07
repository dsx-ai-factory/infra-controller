// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package conflict

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestPromoterConfig_ApplyDefaults(t *testing.T) {
	tests := []struct {
		name     string
		input    PromoterConfig
		expected PromoterConfig
	}{
		{
			name:  "zero values get defaults",
			input: PromoterConfig{},
			expected: PromoterConfig{
				SweepInterval:     defaultSweepInterval,
				NotifyChannelSize: defaultNotifyChSize,
				RestartBackoff:    defaultRestartBackoff,
			},
		},
		{
			name: "explicit values are preserved",
			input: PromoterConfig{
				SweepInterval:     2 * time.Minute,
				NotifyChannelSize: 128,
				RestartBackoff:    10 * time.Second,
			},
			expected: PromoterConfig{
				SweepInterval:     2 * time.Minute,
				NotifyChannelSize: 128,
				RestartBackoff:    10 * time.Second,
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			tc.input.applyDefaults()
			assert.Equal(t, tc.expected.SweepInterval, tc.input.SweepInterval)
			assert.Equal(t, tc.expected.NotifyChannelSize, tc.input.NotifyChannelSize)
			assert.Equal(t, tc.expected.RestartBackoff, tc.input.RestartBackoff)
		})
	}
}

func TestPromoter_Notify(t *testing.T) {
	t.Run("sends rack ID to channel", func(t *testing.T) {
		p := NewPromoter(
			newMockStore(),
			func(_ context.Context, _ uuid.UUID) error { return nil },
			PromoterConfig{NotifyChannelSize: 4},
		)
		rackID := uuid.New()
		p.Notify(rackID)

		select {
		case got := <-p.notifyCh:
			assert.Equal(t, rackID, got)
		default:
			t.Fatal("expected notification in channel, got none")
		}
	})

	t.Run("drops notification silently when channel is full", func(t *testing.T) {
		p := NewPromoter(
			newMockStore(),
			func(_ context.Context, _ uuid.UUID) error { return nil },
			PromoterConfig{NotifyChannelSize: 1},
		)
		rackID := uuid.New()
		p.Notify(rackID) // fills the channel
		p.Notify(rackID) // must not block or panic

		count := 0
		for {
			select {
			case <-p.notifyCh:
				count++
			default:
				assert.Equal(t, 1, count)
				return
			}
		}
	})
}

func TestPromoter_ProcessRack(t *testing.T) {
	future := time.Now().Add(1 * time.Hour)
	past := time.Now().Add(-1 * time.Minute)

	tests := []struct {
		name              string
		setupStore        func(*mockStore, uuid.UUID)
		wantStatusUpdates []taskdef.TaskStatusUpdate // ID + Status pairs to check
		wantPromotedCount int
	}{
		{
			name:              "no waiting tasks — nothing happens",
			setupStore:        func(_ *mockStore, _ uuid.UUID) {},
			wantStatusUpdates: nil,
			wantPromotedCount: 0,
		},
		{
			name: "expired task is terminated",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				t := &taskdef.Task{
					ID:             uuid.New(),
					RackID:         rackID,
					QueueExpiresAt: &past,
					Status:         taskcommon.TaskStatusWaiting,
				}
				s.waitingTasks[rackID] = []*taskdef.Task{t}
			},
			wantStatusUpdates: []taskdef.TaskStatusUpdate{
				{Status: taskcommon.TaskStatusTerminated},
			},
			wantPromotedCount: 0,
		},
		{
			name: "non-expired task promoted when rack is free",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				t := &taskdef.Task{
					ID:             uuid.New(),
					RackID:         rackID,
					QueueExpiresAt: &future,
					Status:         taskcommon.TaskStatusWaiting,
				}
				s.waitingTasks[rackID] = []*taskdef.Task{t}
			},
			wantStatusUpdates: []taskdef.TaskStatusUpdate{
				{Status: taskcommon.TaskStatusPending},
			},
			wantPromotedCount: 1,
		},
		{
			name: "task with no expiry is promoted when rack is free",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				t := &taskdef.Task{
					ID:             uuid.New(),
					RackID:         rackID,
					QueueExpiresAt: nil,
					Status:         taskcommon.TaskStatusWaiting,
				}
				s.waitingTasks[rackID] = []*taskdef.Task{t}
			},
			wantStatusUpdates: []taskdef.TaskStatusUpdate{
				{Status: taskcommon.TaskStatusPending},
			},
			wantPromotedCount: 1,
		},
		{
			name: "no promotion when rack has conflicting active task",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				waiting := makeTaskWithType(
					rackID,
					taskcommon.TaskTypePowerControl, "power_on",
					devicetypes.ComponentTypePowerShelf, uuid.New(),
				)
				waiting.QueueExpiresAt = &future
				waiting.Status = taskcommon.TaskStatusWaiting
				s.waitingTasks[rackID] = []*taskdef.Task{waiting}
				s.activeTasks[rackID] = []*taskdef.Task{
					makeTaskWithType(rackID,
						taskcommon.TaskTypePowerControl, "power_on",
						devicetypes.ComponentTypePowerShelf,
						uuid.New()),
				}
			},
			wantStatusUpdates: nil,
			wantPromotedCount: 0,
		},
		{
			name: "mixed expired and valid — expired terminated, valid promoted",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				expired := &taskdef.Task{
					ID:             uuid.New(),
					RackID:         rackID,
					QueueExpiresAt: &past,
					Status:         taskcommon.TaskStatusWaiting,
				}
				valid := &taskdef.Task{
					ID:             uuid.New(),
					RackID:         rackID,
					QueueExpiresAt: &future,
					Status:         taskcommon.TaskStatusWaiting,
				}
				s.waitingTasks[rackID] = []*taskdef.Task{expired, valid}
			},
			wantStatusUpdates: []taskdef.TaskStatusUpdate{
				{Status: taskcommon.TaskStatusTerminated},
				{Status: taskcommon.TaskStatusPending},
			},
			wantPromotedCount: 1,
		},
		{
			name: "FIFO — only oldest promoted when ops conflict",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				// Both are PowerShelf power_control: they conflict
				// at rack scope. Older is promoted; newer blocks
				// the queue so nothing behind it runs.
				older := makeTaskWithType(
					rackID,
					taskcommon.TaskTypePowerControl, "power_on",
					devicetypes.ComponentTypePowerShelf, uuid.New(),
				)
				older.QueueExpiresAt = &future
				older.Status = taskcommon.TaskStatusWaiting
				newer := makeTaskWithType(
					rackID,
					taskcommon.TaskTypePowerControl, "power_on",
					devicetypes.ComponentTypePowerShelf, uuid.New(),
				)
				newer.QueueExpiresAt = &future
				newer.Status = taskcommon.TaskStatusWaiting
				s.waitingTasks[rackID] = []*taskdef.Task{
					older, newer,
				}
			},
			wantStatusUpdates: []taskdef.TaskStatusUpdate{
				{Status: taskcommon.TaskStatusPending},
			},
			wantPromotedCount: 1,
		},
		{
			name: "multiple non-conflicting tasks promoted in one pass",
			setupStore: func(s *mockStore, rackID uuid.UUID) {
				// inject_expectation does not appear in any
				// builtinRule pair, so two such tasks
				// can be promoted simultaneously.
				t1 := makeTask(
					rackID,
					taskcommon.TaskTypeInjectExpectation,
					"inject",
				)
				t1.QueueExpiresAt = &future
				t1.Status = taskcommon.TaskStatusWaiting
				t2 := makeTask(
					rackID,
					taskcommon.TaskTypeInjectExpectation,
					"inject",
				)
				t2.QueueExpiresAt = &future
				t2.Status = taskcommon.TaskStatusWaiting
				s.waitingTasks[rackID] = []*taskdef.Task{t1, t2}
			},
			wantStatusUpdates: []taskdef.TaskStatusUpdate{
				{Status: taskcommon.TaskStatusPending},
				{Status: taskcommon.TaskStatusPending},
			},
			wantPromotedCount: 2,
		},
		{
			name: "list waiting error — no panics, no updates",
			setupStore: func(s *mockStore, _ uuid.UUID) {
				s.listWaitingErr = assert.AnError
			},
			wantStatusUpdates: nil,
			wantPromotedCount: 0,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := newMockStore()
			rackID := uuid.New()
			tc.setupStore(store, rackID)

			promotedCount := 0
			p := NewPromoter(store, func(_ context.Context, _ uuid.UUID) error {
				promotedCount++
				return nil
			}, PromoterConfig{})

			p.processRack(context.Background(), rackID)

			require.Len(t, store.statusUpdates, len(tc.wantStatusUpdates))
			for i, want := range tc.wantStatusUpdates {
				assert.Equal(t, want.Status, store.statusUpdates[i].Status)
			}
			assert.Equal(t, tc.wantPromotedCount, promotedCount)
		})
	}
}

func TestPromoter_SweepAllRacks(t *testing.T) {
	future := time.Now().Add(1 * time.Hour)

	tests := []struct {
		name              string
		setupStore        func(*mockStore)
		wantPromotedCount int
	}{
		{
			name:              "no racks with waiting tasks",
			setupStore:        func(s *mockStore) { s.racksWithWaiting = []uuid.UUID{} },
			wantPromotedCount: 0,
		},
		{
			name: "processes each rack independently",
			setupStore: func(s *mockStore) {
				rack1 := uuid.New()
				rack2 := uuid.New()
				s.racksWithWaiting = []uuid.UUID{rack1, rack2}
				s.waitingTasks[rack1] = []*taskdef.Task{
					{ID: uuid.New(), RackID: rack1, QueueExpiresAt: &future, Status: taskcommon.TaskStatusWaiting},
				}
				s.waitingTasks[rack2] = []*taskdef.Task{
					{ID: uuid.New(), RackID: rack2, QueueExpiresAt: &future, Status: taskcommon.TaskStatusWaiting},
				}
			},
			wantPromotedCount: 2,
		},
		{
			name: "list racks error — no panics, nothing promoted",
			setupStore: func(s *mockStore) {
				s.listRacksErr = assert.AnError
			},
			wantPromotedCount: 0,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := newMockStore()
			tc.setupStore(store)

			promotedCount := 0
			p := NewPromoter(store, func(_ context.Context, _ uuid.UUID) error {
				promotedCount++
				return nil
			}, PromoterConfig{})

			p.sweepAllRacks(context.Background())

			assert.Equal(t, tc.wantPromotedCount, promotedCount)
		})
	}
}
