// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestTrayStatusCounts_Add(t *testing.T) {
	tests := []struct {
		name     string
		statuses []*ComponentOperationStatus
		want     TrayStatusCounts
	}{
		{name: "empty membership"},
		{
			name: "unknown does not truncate remaining counts",
			statuses: []*ComponentOperationStatus{
				nil, {Phase: Phase("future")}, {Phase: PhaseUnknown},
				{Phase: PhaseError}, {Phase: PhaseReady}, {Phase: PhaseReady},
				{Phase: PhaseInUse}, {Phase: PhaseDeleting}, {Phase: PhaseInitializing},
			},
			want: TrayStatusCounts{Ready: 2, InUse: 1, Initializing: 1, Error: 1, Deleting: 1, Unknown: 3},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var got TrayStatusCounts
			for _, status := range tt.statuses {
				got.Add(status)
			}
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestRackOperationStatusDetails_ToProto(t *testing.T) {
	tests := []struct {
		name    string
		details *RackOperationStatusDetails
	}{
		{name: "unavailable"},
		{name: "empty", details: &RackOperationStatusDetails{}},
		{name: "populated", details: &RackOperationStatusDetails{TrayCountsByType: TrayStatusCountsByType{
			Compute:    TrayStatusCounts{Ready: 6, InUse: 5},
			NVSwitch:   TrayStatusCounts{Initializing: 4, Error: 3},
			PowerShelf: TrayStatusCounts{Deleting: 2, Unknown: 1},
		}}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			wire := tt.details.ToProto()
			if tt.details == nil {
				assert.Nil(t, wire)
				return
			}
			var got RackOperationStatusDetails
			got.FromProto(wire)
			assert.Equal(t, *tt.details, got)
		})
	}
}

func TestTrayStatusCountsByType_Phase(t *testing.T) {
	tests := []struct {
		name   string
		counts TrayStatusCountsByType
		want   Phase
	}{
		{name: "empty rack", want: PhaseUnknown},
		{name: "empty types do not mask ready", counts: TrayStatusCountsByType{Compute: TrayStatusCounts{Ready: 1}}, want: PhaseReady},
		{name: "error in another type", counts: TrayStatusCountsByType{Compute: TrayStatusCounts{Ready: 1}, NVSwitch: TrayStatusCounts{Error: 1}}, want: PhaseError},
		{name: "unknown tray overrides error", counts: TrayStatusCountsByType{NVSwitch: TrayStatusCounts{Error: 1}, PowerShelf: TrayStatusCounts{Unknown: 1}}, want: PhaseUnknown},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.counts.Phase())
		})
	}
}
