// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

func TestLane_claim(t *testing.T) {
	t.Run("bounds claims by available capacity", func(t *testing.T) {
		store := newFakeStore()
		workLane := newLane(
			"pending",
			LaneConfig{Workers: 2, ScanLimit: 10},
			store.ClaimPendingExecutions,
		)

		_, err := workLane.claim(context.Background(), testInstanceID, time.Minute, 4)
		require.NoError(t, err)
		require.Len(t, store.requests, 1)
		require.Equal(t, 2, store.requests[0].Limit)
		require.Len(t, workLane.slots, 2)

		<-workLane.slots

		_, err = workLane.claim(context.Background(), testInstanceID, time.Minute, 4)
		require.NoError(t, err)
		require.Len(t, store.requests, 2)
		require.Equal(t, 1, store.requests[1].Limit)
	})

	t.Run("normalizes claim cancellation", func(t *testing.T) {
		store := newFakeStore()
		workLane := newLane(
			"pending",
			LaneConfig{Workers: 1, ScanLimit: 1},
			store.ClaimPendingExecutions,
		)
		ctx, cancel := context.WithCancel(context.Background())
		cancel()

		_, err := workLane.claim(ctx, testInstanceID, time.Minute, 4)
		require.NoError(t, err)
		require.Len(t, workLane.slots, 1)
	})

	t.Run("preserves a store error during cancellation", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		workLane := newLane(
			"pending",
			LaneConfig{Workers: 1, ScanLimit: 1},
			func(
				context.Context,
				eventrule.ExecutionClaimRequest,
			) (eventrule.ExecutionClaimBatch, error) {
				cancel()

				return eventrule.ExecutionClaimBatch{}, errors.New("store unavailable")
			},
		)
		_, err := workLane.claim(ctx, testInstanceID, time.Minute, 4)

		require.EqualError(t, err, "claim pending executions: store unavailable")
		require.Len(t, workLane.slots, 1)
	})
}
