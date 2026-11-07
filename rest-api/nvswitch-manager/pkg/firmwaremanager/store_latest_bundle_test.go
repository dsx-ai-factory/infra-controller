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

func TestInMemoryUpdateStore_GetLatestBundleBySwitch(t *testing.T) {
	switchID, otherSwitchID := uuid.New(), uuid.New()
	latestBundleID, staleBundleID, otherBundleID := uuid.New(), uuid.New(), uuid.New()
	baseTime := time.Date(2026, time.October, 19, 12, 0, 0, 0, time.UTC)
	newUpdate := func(switchID uuid.UUID, bundleID *uuid.UUID, minute int) *FirmwareUpdate {
		if bundleID != nil {
			id := *bundleID
			bundleID = &id
		}
		return &FirmwareUpdate{
			ID:             uuid.New(),
			SwitchUUID:     switchID,
			BundleUpdateID: bundleID,
			CreatedAt:      baseTime.Add(time.Duration(minute) * time.Minute),
		}
	}

	oldStandalone := newUpdate(switchID, nil, 1)
	// Every member of the selected bundle belongs in the result, even one
	// created before a record in a stale bundle.
	latestBundleFirst := newUpdate(switchID, &latestBundleID, 2)
	staleBundle := newUpdate(switchID, &staleBundleID, 3)
	latestBundleLast := newUpdate(switchID, &latestBundleID, 4)
	latestStandalone := newUpdate(switchID, nil, 5)
	foreignSameBundle := newUpdate(otherSwitchID, &latestBundleID, 6)
	foreignNewerBundle := newUpdate(otherSwitchID, &otherBundleID, 7)
	foreignStandalone := newUpdate(otherSwitchID, nil, 8)

	tests := []struct {
		name    string
		updates []*FirmwareUpdate
		wantIDs []uuid.UUID
	}{
		{
			name:    "no records for requested switch",
			updates: []*FirmwareUpdate{foreignStandalone, foreignNewerBundle},
			wantIDs: []uuid.UUID{},
		},
		{
			name:    "standalone history returns only newest",
			updates: []*FirmwareUpdate{latestStandalone, oldStandalone},
			wantIDs: []uuid.UUID{latestStandalone.ID},
		},
		{
			name: "mixed history returns latest bundle and latest standalone",
			updates: []*FirmwareUpdate{
				latestBundleLast, foreignSameBundle, latestStandalone, staleBundle,
				foreignNewerBundle, latestBundleFirst, oldStandalone, foreignStandalone,
			},
			wantIDs: []uuid.UUID{latestBundleFirst.ID, latestBundleLast.ID, latestStandalone.ID},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := NewInMemoryUpdateStore()
			ctx := t.Context()
			for _, update := range tt.updates {
				err := store.Save(ctx, update)
				require.NoError(t, err)
			}

			updates, err := store.GetLatestBundleBySwitch(ctx, switchID)
			require.NoError(t, err)
			gotIDs := make([]uuid.UUID, len(updates))
			for i, update := range updates {
				require.NotNil(t, update)
				gotIDs[i] = update.ID
			}
			assert.ElementsMatch(t, tt.wantIDs, gotIDs)
		})
	}
}
