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
		{name: "populated", details: &RackOperationStatusDetails{TrayCounts: TrayStatusCounts{
			Ready: 6, InUse: 5, Initializing: 4, Error: 3, Deleting: 2, Unknown: 1,
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
