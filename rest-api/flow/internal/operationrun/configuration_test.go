// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

func TestOperationValidateAcceptsValidOperation(t *testing.T) {
	op := validOperation()

	require.NoError(t, op.Validate())
}

func TestOperationValidateRejectsInvalidTargetScope(t *testing.T) {
	op := validOperation()
	op.TargetScope.ExcludedOperationRunIDs = []uuid.UUID{uuid.Nil}

	err := op.Validate()
	require.ErrorContains(t, err, "target_scope: excluded_operation_run_ids[0] is required")
}

func TestOperationValidateRejectsInvalidTargetSpec(t *testing.T) {
	op := validOperation()
	op.TargetSpec = &operation.TargetSpec{}

	err := op.Validate()
	require.ErrorContains(t, err, "target_spec: target_spec must have exactly one of racks, nvl_domains, or components set")
}

func TestOperationValidateRejectsDefaultScopeFilterWithTargetSpec(t *testing.T) {
	op := validOperation()
	op.TargetScope.DefaultScopeComponentFilter = &operation.ComponentFilter{
		Kind:  operation.ComponentFilterKindTypes,
		Types: []string{"Compute"},
	}

	err := op.Validate()
	require.ErrorContains(
		t,
		err,
		"target_scope.default_scope_component_filter requires target_spec to be omitted",
	)
}

func TestOperationValidateAcceptsDefaultScopeFilterWithoutTargetSpec(t *testing.T) {
	op := validOperation()
	op.TargetSpec = nil
	op.TargetScope.DefaultScopeComponentFilter = &operation.ComponentFilter{
		Kind:  operation.ComponentFilterKindTypes,
		Types: []string{"Compute"},
	}

	require.NoError(t, op.Validate())
}

func TestOperationValidateRejectsPayloadMismatch(t *testing.T) {
	op := validOperation()
	op.Type = taskcommon.TaskTypePowerControl

	err := op.Validate()
	require.ErrorContains(t, err, "operation type does not match payload")

	op = validOperation()
	op.Code = taskcommon.OpCodePowerControlPowerOn

	err = op.Validate()
	require.ErrorContains(t, err, "operation code does not match payload")
}

func TestOptionsValidateAcceptsValidOptions(t *testing.T) {
	opts := validOptions()

	require.NoError(t, opts.Validate())
}

func TestOptionsValidateRejectsInvalidNestedPolicy(t *testing.T) {
	opts := validOptions()
	opts.PhasePolicy = PhasePolicy{
		Plan: &PercentagePhases{
			Phases: []PercentagePhase{{Percentage: 50}},
		},
	}

	err := opts.Validate()
	require.ErrorContains(
		t,
		err,
		"phase_policy: percentage phase percentages must sum to 100",
	)
}

func TestSelectorValidateRejectsInvalidPercentageSelector(t *testing.T) {
	selector := &PercentageSelector{Percentage: 0, Seed: "selector-seed"}

	err := selector.Validate()
	require.ErrorContains(t, err, "percentage selector must be between 1 and 100")
}

func TestOperationRunDecodedSelectorValidatesConfig(t *testing.T) {
	raw, err := MarshalConfig(&PercentageSelector{Percentage: 0, Seed: "selector-seed"})
	require.NoError(t, err)

	_, err = (&OperationRun{Selector: raw}).DecodedSelector()
	require.ErrorContains(
		t,
		err,
		"validate operation run selector: percentage selector must be between 1 and 100",
	)
}

func TestOperationRunDecodedOptionsValidatesConfig(t *testing.T) {
	opts := validOptions()
	opts.MaxConcurrentTargets = 0
	raw, err := MarshalConfig(opts)
	require.NoError(t, err)

	_, err = (&OperationRun{Options: raw}).DecodedOptions()
	require.ErrorContains(
		t,
		err,
		"validate operation run options: max_concurrent_targets must be greater than 0",
	)
}

func validOperation() *Operation {
	return &Operation{
		Type: taskcommon.TaskTypeFirmwareControl,
		Code: taskcommon.OpCodeFirmwareControlUpgrade,
		TargetSpec: &operation.TargetSpec{
			Components: []operation.ComponentTarget{
				{UUID: uuid.MustParse("00000000-0000-0000-0000-000000000001")},
			},
		},
		TargetScope: OperationTargetScope{
			ExcludedOperationRunIDs: []uuid.UUID{
				uuid.MustParse("00000000-0000-0000-0000-000000000002"),
			},
		},
		Payload: &operations.FirmwareControlTaskInfo{
			Operation: operations.FirmwareOperationUpgrade,
		},
	}
}

func validOptions() *Options {
	return &Options{
		MaxConcurrentTargets: 1,
		ConflictPolicy: ConflictPolicy{
			Payload: &ConflictRetryPolicy{
				RetryTimeout:      time.Hour,
				InitialRetryDelay: time.Second,
				MaxRetryDelay:     time.Minute,
			},
		},
		OrderingPolicy: OrderingPolicy{
			Payload: &RandomOrdering{Seed: "ordering-seed"},
		},
		PhasePolicy: PhasePolicy{
			Plan: &EqualPhases{PhaseCount: 1},
		},
	}
}
