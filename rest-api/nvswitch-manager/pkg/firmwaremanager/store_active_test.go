// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestInMemoryUpdateStore_GetActive(t *testing.T) {
	tests := []struct {
		name           string
		matchingStates []UpdateState
		wantActive     bool
	}{
		{
			name:           "queued matching work blocks",
			matchingStates: []UpdateState{StateQueued},
			wantActive:     true,
		},
		{
			name:           "in-progress matching work is returned",
			matchingStates: []UpdateState{StateInstall},
			wantActive:     true,
		},
		{
			name:           "only terminal matching work does not block",
			matchingStates: []UpdateState{StateCompleted, StateFailed, StateCancelled},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := t.Context()
			store := NewInMemoryUpdateStore()
			switchID := uuid.New()
			updates := []*FirmwareUpdate{
				{ID: uuid.New(), SwitchUUID: uuid.New(), Component: nvswitch.NVOS, State: StateInstall},
				{ID: uuid.New(), SwitchUUID: switchID, Component: nvswitch.CPLD, State: StateQueued},
			}
			var wantID uuid.UUID
			for _, state := range tt.matchingStates {
				update := &FirmwareUpdate{
					ID:         uuid.New(),
					SwitchUUID: switchID,
					Component:  nvswitch.NVOS,
					State:      state,
				}
				updates = append(updates, update)
				if tt.wantActive {
					wantID = update.ID
				}
			}
			for _, update := range updates {
				err := store.Save(ctx, update)
				require.NoError(t, err)
			}

			got, err := store.GetActive(ctx, switchID, nvswitch.NVOS)
			require.NoError(t, err)
			if tt.wantActive {
				require.NotNil(t, got)
				assert.Equal(t, wantID, got.ID)
			} else {
				assert.Nil(t, got)
			}
		})
	}
}
