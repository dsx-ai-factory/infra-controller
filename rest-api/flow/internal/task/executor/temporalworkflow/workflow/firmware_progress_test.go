// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"
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

func TestWithFirmwareProgressReporter(t *testing.T) {
	var suite testsuite.WorkflowTestSuite
	env := suite.NewTestWorkflowEnvironment()
	env.RegisterWorkflowWithOptions(func(ctx workflow.Context) (bool, error) {
		step := operationrules.SequenceStep{Stage: 1, MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl}}
		target := common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"X"}}
		normal := withFirmwareProgressReporter(ctx, step, target)
		enabled := normal.Value(firmwareProgressContextKey{}) != nil
		isolated := withFirmwareProgressReporter(workflow.WithValue(ctx, isolatedActionContextKey{}, true), step, target)
		return enabled && isolated.Value(firmwareProgressContextKey{}) == nil, nil
	}, workflow.RegisterOptions{Name: "testFirmwareReporterChild"})
	env.ExecuteWorkflow(func(ctx workflow.Context) (bool, error) {
		var suppressed bool
		err := workflow.ExecuteChildWorkflow(ctx, "testFirmwareReporterChild").Get(ctx, &suppressed)
		return suppressed, err
	})
	require.NoError(t, env.GetWorkflowError())
	var suppressed bool
	require.NoError(t, env.GetWorkflowResult(&suppressed))
	require.True(t, suppressed, "normal children report progress; isolated children suppress it")
}
