// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestFirmwareProgressAccumulator_Update(t *testing.T) {
	completed := operations.FirmwareUpdateStateCompleted
	failed := operations.FirmwareUpdateStateFailed
	queued := operations.FirmwareUpdateStateQueued

	tests := []struct {
		name    string
		updates []struct {
			target   []string
			statuses map[string]operations.FirmwareUpdateStatus
			want     firmwareStepProgress
		}
	}{
		{
			name: "missing status removes the prior terminal outcome for the current batch",
			updates: []struct {
				target   []string
				statuses map[string]operations.FirmwareUpdateStatus
				want     firmwareStepProgress
			}{
				{
					target: []string{"comp-a", "comp-b"},
					statuses: map[string]operations.FirmwareUpdateStatus{
						"comp-a": firmwareStatus("comp-a", completed),
					},
					want: firmwareStepProgress{CompletedComponents: 1},
				},
				{
					target: []string{"comp-a", "comp-b"},
					statuses: map[string]operations.FirmwareUpdateStatus{
						"comp-b": firmwareStatus("comp-b", completed),
					},
					want: firmwareStepProgress{CompletedComponents: 1},
				},
			},
		},
		{
			name: "terminal outcomes from completed batches remain aggregated",
			updates: []struct {
				target   []string
				statuses map[string]operations.FirmwareUpdateStatus
				want     firmwareStepProgress
			}{
				{
					target: []string{"comp-a"},
					statuses: map[string]operations.FirmwareUpdateStatus{
						"comp-a": firmwareStatus("comp-a", completed),
					},
					want: firmwareStepProgress{CompletedComponents: 1},
				},
				{
					target: []string{"comp-b", "comp-c"},
					statuses: map[string]operations.FirmwareUpdateStatus{
						"comp-b": firmwareStatus("comp-b", failed),
						"comp-c": firmwareStatus("comp-c", queued),
						"other":  firmwareStatus("other", completed),
					},
					want: firmwareStepProgress{
						CompletedComponents: 1,
						FailedComponents:    1,
					},
				},
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			accumulator := firmwareProgressAccumulator{
				stageNumber:   2,
				componentType: "Compute",
				terminalState: make(map[string]operations.FirmwareUpdateState),
			}
			for _, update := range tc.updates {
				got := accumulator.update(common.Target{
					Type:        devicetypes.ComponentTypeCompute,
					Identifiers: update.target,
				}, update.statuses)
				update.want.StageNumber = 2
				update.want.ComponentType = "Compute"
				require.Equal(t, update.want, got)
			}
		})
	}
}
