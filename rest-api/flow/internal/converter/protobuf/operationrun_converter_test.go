// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	opmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestOperationRunFromDefaults(t *testing.T) {
	req := validCreateRequest()

	run, err := OperationRunFrom(req)
	require.NoError(t, err)

	require.Equal(t, "firmware canary", run.Name)
	require.Equal(t, taskcommon.TaskTypeFirmwareControl, run.OperationType)
	require.Equal(t, taskcommon.OpCodeFirmwareControlUpgrade, run.OperationCode)

	selector := mustUnmarshalSelector(t, run.Selector)
	require.Equal(t, operationrun.SelectorKindPercentage, selector.SelectorKind())
	percentageSelector, ok := selector.(*operationrun.PercentageSelector)
	require.True(t, ok)
	require.EqualValues(t, 10, percentageSelector.Percentage)
	require.NotEmpty(t, percentageSelector.Seed)

	options := mustUnmarshalOptions(t, run.Options)
	require.EqualValues(t, 2, options.MaxConcurrentTargets)
	failureRate, ok := options.SafetyPolicy.Gates[0].(*operationrun.FailureRateGate)
	require.True(t, ok)
	require.Equal(
		t,
		operationrun.SafetyGateScopeCurrentPhase,
		failureRate.Scope,
	)
	retry, ok := options.ConflictPolicy.Payload.(*operationrun.ConflictRetryPolicy)
	require.True(t, ok)
	require.NotZero(t, retry.RetryTimeout)
	random, ok := options.OrderingPolicy.Payload.(*operationrun.RandomOrdering)
	require.True(t, ok)
	require.NotEmpty(t, random.Seed)
	equal, ok := options.PhasePolicy.Plan.(*operationrun.EqualPhases)
	require.True(t, ok)
	require.EqualValues(t, 1, equal.PhaseCount)
	require.False(t, options.PhasePolicy.AdvancePolicy.AutoAdvance)
	operation := mustUnmarshalOperation(t, run.OperationTemplate)
	require.Equal(t, taskcommon.TaskTypeFirmwareControl, operation.Type)
	require.Equal(t, taskcommon.OpCodeFirmwareControlUpgrade, operation.Code)
	require.Equal(t, "roll rack firmware", operation.Description)
	require.NotNil(t, operation.QueueOptions)
	require.Equal(
		t,
		opmodel.ConflictStrategyQueue,
		operation.QueueOptions.ConflictStrategy,
	)
	require.EqualValues(t, 300, operation.QueueOptions.QueueTimeoutSeconds)
	require.Empty(t, operation.TargetScope.ExcludedOperationRunIDs)
	payload, ok := operation.Payload.(*operations.FirmwareControlTaskInfo)
	require.True(t, ok)
	require.Equal(t, operations.FirmwareOperationUpgrade, payload.Operation)
	require.Equal(t, "1.2.3", payload.TargetVersion)
	require.Equal(t, "33333333-3333-3333-3333-333333333333", payload.RuleID)
	require.Equal(t, []string{"bmc"}, payload.SubTargets)
	require.True(t, payload.OverrideReadinessCheck)
	require.True(t, payload.OverrideVersionCheck)
	require.Equal(t, operationrun.OperationRunStatusPending, run.Status)
	require.Equal(t, operationrun.OperationRunStatusReasonNone, run.StatusReason)
	require.NotEmpty(t, run.Selector)
	require.NotEmpty(t, run.Options)
	require.NotEmpty(t, run.OperationTemplate)
}

func TestOperationRunToRebuildsConfigurationFromInternalJSON(t *testing.T) {
	run, err := OperationRunFrom(validCreateRequest())
	require.NoError(t, err)
	run.TotalPhases = 2

	got, err := OperationRunTo(run)
	require.NoError(t, err)

	require.NotNil(t, got.GetConfiguration())
	require.Equal(t, int32(2), got.GetSummary().GetTotalPhases())
	require.EqualValues(
		t,
		10,
		got.GetConfiguration().GetSelector().GetPercentage().GetPercentage(),
	)
	require.NotEmpty(
		t,
		got.GetConfiguration().GetSelector().GetPercentage().GetSeed(),
	)
	require.EqualValues(
		t,
		2,
		got.GetConfiguration().GetOptions().GetMaxConcurrentTargets(),
	)
	require.NotNil(
		t,
		got.GetConfiguration().
			GetOptions().
			GetConflictPolicy().
			GetRetry().
			GetRetryTimeout(),
	)
	require.Equal(
		t,
		"1.2.3",
		got.GetConfiguration().
			GetOperation().
			GetUpgradeFirmware().
			GetTargetVersion(),
	)
	require.Equal(
		t,
		pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE,
		got.GetConfiguration().
			GetOperation().
			GetUpgradeFirmware().
			GetQueueOptions().
			GetConflictStrategy(),
	)
	require.Equal(
		t,
		"33333333-3333-3333-3333-333333333333",
		got.GetConfiguration().
			GetOperation().
			GetUpgradeFirmware().
			GetRuleId().
			GetId(),
	)
	require.True(t, got.GetConfiguration().GetOperation().GetUpgradeFirmware().GetOverrideVersionCheck())
}

func TestOperationRunNVLDomainTargetRoundTrip(t *testing.T) {
	domainID := uuid.New()
	req := validCreateRequest()
	req.Configuration.Operation.GetUpgradeFirmware().TargetSpec = &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_NvlDomains{
			NvlDomains: &pb.NVLDomainTargets{
				Targets: []*pb.NVLDomainTarget{
					{
						Identifier: &pb.NVLDomainTarget_Id{
							Id: &pb.UUID{Id: domainID.String()},
						},
						ComponentTypes: []pb.ComponentType{
							pb.ComponentType_COMPONENT_TYPE_COMPUTE,
						},
					},
				},
			},
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)
	operation := mustUnmarshalOperation(t, run.OperationTemplate)
	require.NotNil(t, operation.TargetSpec)
	require.Len(t, operation.TargetSpec.NVLDomains, 1)
	require.Equal(t, domainID, operation.TargetSpec.NVLDomains[0].Identifier.ID)

	got, err := OperationRunTo(run)
	require.NoError(t, err)
	targets := got.GetConfiguration().GetOperation().GetUpgradeFirmware().GetTargetSpec().GetNvlDomains()
	require.Len(t, targets.GetTargets(), 1)
	require.Equal(t, domainID.String(), targets.GetTargets()[0].GetId().GetId())
	require.Equal(
		t,
		[]pb.ComponentType{pb.ComponentType_COMPONENT_TYPE_COMPUTE},
		targets.GetTargets()[0].GetComponentTypes(),
	)
}

func TestOperationRunSummaryToUsesMaterializedPhaseCount(t *testing.T) {
	run := &operationrun.OperationRun{
		TotalPhases: 2,
		Options:     []byte("{"),
	}

	got, err := OperationRunSummaryTo(run)

	require.NoError(t, err)
	require.Equal(t, int32(2), got.GetTotalPhases())
}

func TestOperationRunStatusConversionIncludesCompletedWithFailures(t *testing.T) {
	require.Equal(
		t,
		pb.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED_WITH_FAILURES,
		OperationRunStatusTo(operationrun.OperationRunStatusCompletedWithFailures),
	)
	require.Equal(
		t,
		operationrun.OperationRunStatusCompletedWithFailures,
		OperationRunStatusFrom(
			pb.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED_WITH_FAILURES,
		),
	)
}

func TestOperationRunFromRejectsMissingConfiguration(t *testing.T) {
	req := &pb.CreateOperationRunRequest{Name: "firmware canary"}

	_, err := OperationRunFrom(req)
	require.ErrorContains(t, err, "configuration is required")
}

func TestOperationRunFromRejectsPhysicalLocationOrdering(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.OrderingPolicy = &pb.OperationRunOrderingPolicy{
		Ordering: &pb.OperationRunOrderingPolicy_PhysicalLocation{
			PhysicalLocation: &pb.OperationRunPhysicalLocationOrdering{
				Strategy: pb.OperationRunPhysicalLocationOrdering_STRATEGY_ROW_BY_ROW,
			},
		},
	}

	_, err := OperationRunFrom(req)
	require.ErrorContains(t, err, "physical_location ordering is not supported yet")
}

func TestOperationRunFromPreservesPercentagePhases(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.PhasePolicy = &pb.OperationRunPhasePolicy{
		Plan: &pb.OperationRunPhasePolicy_Percentage{
			Percentage: &pb.PercentageOperationRunPhases{
				Phases: []*pb.OperationRunPercentagePhase{
					{Percentage: 10},
					{Percentage: 90},
				},
			},
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)
	options := mustUnmarshalOptions(t, run.Options)
	percentage, ok := options.PhasePolicy.Plan.(*operationrun.PercentagePhases)
	require.True(t, ok)
	require.Len(t, percentage.Phases, 2)

	require.NotEmpty(t, run.Options)
}

func TestOperationRunFromPreservesCountPhases(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.PhasePolicy = &pb.OperationRunPhasePolicy{
		Plan: &pb.OperationRunPhasePolicy_Count{
			Count: &pb.CountOperationRunPhases{
				Phases: []*pb.OperationRunCountPhase{
					{Count: 5},
					{Count: 10},
				},
			},
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)
	options := mustUnmarshalOptions(t, run.Options)
	count, ok := options.PhasePolicy.Plan.(*operationrun.CountPhases)
	require.True(t, ok)
	require.Len(t, count.Phases, 2)

	require.NotEmpty(t, run.Options)
}

func TestOperationRunFromRejectsInvalidCountPhases(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.PhasePolicy = &pb.OperationRunPhasePolicy{
		Plan: &pb.OperationRunPhasePolicy_Count{
			Count: &pb.CountOperationRunPhases{
				Phases: []*pb.OperationRunCountPhase{{Count: 0}},
			},
		},
	}

	_, err := OperationRunFrom(req)
	require.ErrorContains(t, err, "count phase counts must be greater than 0")
}

func TestOperationRunFromPreservesAutoAdvance(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.PhasePolicy = &pb.OperationRunPhasePolicy{
		AdvancePolicy: &pb.OperationRunPhaseAdvancePolicy{
			AutoAdvance: true,
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)
	options := mustUnmarshalOptions(t, run.Options)
	require.True(
		t,
		options.PhasePolicy.AdvancePolicy.AutoAdvance,
	)
}

func TestOperationRunFromPreservesFailureCountGate(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.SafetyPolicy = &pb.OperationRunSafetyPolicy{
		Gates: []*pb.OperationRunSafetyGate{
			{
				Gate: &pb.OperationRunSafetyGate_FailureCount{
					FailureCount: &pb.OperationRunFailureCountGate{
						Scope:                 pb.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CUMULATIVE_RUN,
						FailureThresholdCount: 3,
					},
				},
			},
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)

	options := mustUnmarshalOptions(t, run.Options)
	failureCount, ok := options.SafetyPolicy.Gates[0].(*operationrun.FailureCountGate)
	require.True(t, ok)
	require.Equal(
		t,
		operationrun.SafetyGateScopeCumulativeRun,
		failureCount.Scope,
	)
	require.EqualValues(t, 3, failureCount.FailureThresholdCount)
}

func TestOperationRunFromRejectsInvalidFailureCountGate(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Options.SafetyPolicy = &pb.OperationRunSafetyPolicy{
		Gates: []*pb.OperationRunSafetyGate{
			{
				Gate: &pb.OperationRunSafetyGate_FailureCount{
					FailureCount: &pb.OperationRunFailureCountGate{},
				},
			},
		},
	}

	_, err := OperationRunFrom(req)
	require.ErrorContains(
		t,
		err,
		"failure_count.failure_threshold_count must be greater than 0",
	)
}

func TestOperationRunFromRejectsInvalidOperationRunID(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Operation.TargetScope = &pb.OperationRunTargetScope{
		ExcludeOperationRunIds: []*pb.UUID{{Id: "not-a-uuid"}},
	}

	_, err := OperationRunFrom(req)
	require.ErrorContains(
		t,
		err,
		"target_scope.exclude_operation_run_ids[0] must be a valid UUID",
	)
}

func TestOperationRunFromPreservesTargetScope(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Operation.TargetScope = &pb.OperationRunTargetScope{
		ExcludeOperationRunIds: []*pb.UUID{
			{Id: "22222222-2222-2222-2222-222222222222"},
		},
	}
	req.Configuration.Operation.GetUpgradeFirmware().TargetSpec = &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Racks{
			Racks: &pb.RackTargets{
				Targets: []*pb.RackTarget{
					{
						Identifier: &pb.RackTarget_Id{
							Id: &pb.UUID{Id: "11111111-1111-1111-1111-111111111111"},
						},
					},
				},
			},
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)
	operation := mustUnmarshalOperation(t, run.OperationTemplate)
	targetScope := operation.TargetScope
	require.Len(t, targetScope.ExcludedOperationRunIDs, 1)
	require.Equal(
		t,
		"22222222-2222-2222-2222-222222222222",
		targetScope.ExcludedOperationRunIDs[0].String(),
	)
}

func TestOperationRunFromPreservesDefaultScopeComponentFilter(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Operation.TargetScope = &pb.OperationRunTargetScope{
		DefaultScopeComponentFilter: &pb.ComponentFilter{
			Filter: &pb.ComponentFilter_Types{
				Types: &pb.ComponentTypes{
					Types: []pb.ComponentType{
						pb.ComponentType_COMPONENT_TYPE_COMPUTE,
					},
				},
			},
		},
	}

	run, err := OperationRunFrom(req)
	require.NoError(t, err)

	operation := mustUnmarshalOperation(t, run.OperationTemplate)
	filter := operation.TargetScope.DefaultScopeComponentFilter
	require.NotNil(t, filter)
	require.Equal(t, opmodel.ComponentFilterKindTypes, filter.Kind)
	require.Equal(t, []string{"Compute"}, filter.Types)

	got, err := OperationRunTo(run)
	require.NoError(t, err)
	require.Equal(
		t,
		[]pb.ComponentType{pb.ComponentType_COMPONENT_TYPE_COMPUTE},
		got.GetConfiguration().
			GetOperation().
			GetTargetScope().
			GetDefaultScopeComponentFilter().
			GetTypes().
			GetTypes(),
	)
}

func TestOperationRunFromRejectsDefaultScopeFilterWithTargetSpec(t *testing.T) {
	req := validCreateRequest()
	req.Configuration.Operation.TargetScope = &pb.OperationRunTargetScope{
		DefaultScopeComponentFilter: &pb.ComponentFilter{
			Filter: &pb.ComponentFilter_Types{
				Types: &pb.ComponentTypes{
					Types: []pb.ComponentType{
						pb.ComponentType_COMPONENT_TYPE_COMPUTE,
					},
				},
			},
		},
	}
	req.Configuration.Operation.GetUpgradeFirmware().TargetSpec = &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Racks{
			Racks: &pb.RackTargets{
				Targets: []*pb.RackTarget{
					{
						Identifier: &pb.RackTarget_Id{
							Id: &pb.UUID{Id: "11111111-1111-1111-1111-111111111111"},
						},
					},
				},
			},
		},
	}

	_, err := OperationRunFrom(req)
	require.ErrorContains(
		t,
		err,
		"target_scope.default_scope_component_filter requires target_spec to be omitted",
	)
}

func TestOperationRunTargetToUsesResolvedComponentsByType(t *testing.T) {
	createdAt := time.Date(2026, 6, 16, 1, 2, 3, 0, time.UTC)
	updatedAt := createdAt.Add(time.Minute)
	componentA := uuid.MustParse("aaaaaaaa-0000-0000-0000-000000000001")
	componentB := uuid.MustParse("aaaaaaaa-0000-0000-0000-000000000002")
	componentC := uuid.MustParse("bbbbbbbb-0000-0000-0000-000000000001")

	got, err := OperationRunTargetTo(&operationrun.OperationRunTarget{
		ID:             uuid.New(),
		OperationRunID: uuid.New(),
		RackID:         uuid.New(),
		SequenceIndex:  1,
		PhaseIndex:     2,
		Status:         operationrun.OperationRunTargetStatusPending,
		ComponentsByType: opmodel.ComponentsByType{
			devicetypes.ComponentTypeNVSwitch: {componentC},
			devicetypes.ComponentTypeCompute: {
				componentB,
				componentA,
			},
		},
		CreatedAt: createdAt,
		UpdatedAt: updatedAt,
	})
	require.NoError(t, err)

	groups := got.GetComponentsByType().GetGroups()
	require.Len(t, groups, 2)
	require.Equal(t, pb.ComponentType_COMPONENT_TYPE_COMPUTE, groups[0].GetType())
	require.Equal(t, []*pb.UUID{UUIDTo(componentA), UUIDTo(componentB)}, groups[0].GetComponentIds())
	require.Equal(t, pb.ComponentType_COMPONENT_TYPE_NVSWITCH, groups[1].GetType())
	require.Equal(t, []*pb.UUID{UUIDTo(componentC)}, groups[1].GetComponentIds())
}

func TestOperationRunTargetStatusClaimedRoundTrip(t *testing.T) {
	require.Equal(
		t,
		pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_CLAIMED,
		OperationRunTargetStatusTo(operationrun.OperationRunTargetStatusClaimed),
	)
	require.Equal(
		t,
		operationrun.OperationRunTargetStatusClaimed,
		OperationRunTargetStatusFrom(
			pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_CLAIMED,
		),
	)
}

func TestProgressStatsRoundTrip(t *testing.T) {
	stats := operationrun.ProgressStats{
		CurrentPhase: operationrun.PhaseStats{
			PhaseIndex:      2,
			SelectedTargets: 3,
			StatusCounts: operationrun.TargetStatusCounts{
				Failed:  1,
				Skipped: 1,
			},
		},
		Cumulative: operationrun.PhaseStats{
			PhaseIndex:      2,
			SelectedTargets: 5,
			StatusCounts: operationrun.TargetStatusCounts{
				Completed:  1,
				Failed:     1,
				Terminated: 1,
				Skipped:    1,
			},
		},
	}

	got := ProgressStatsFrom(ProgressStatsTo(stats))
	require.Equal(t, stats, got)
}

func mustUnmarshalSelector(
	t *testing.T,
	raw []byte,
) operationrun.Selector {
	t.Helper()

	var selector operationrun.Selector
	require.NoError(t, operationrun.UnmarshalConfig(raw, &selector))

	return selector
}

func mustUnmarshalOptions(
	t *testing.T,
	raw []byte,
) *operationrun.Options {
	t.Helper()

	options := &operationrun.Options{}
	require.NoError(t, operationrun.UnmarshalConfig(raw, options))

	return options
}

func mustUnmarshalOperation(
	t *testing.T,
	raw []byte,
) *operationrun.Operation {
	t.Helper()

	operation := &operationrun.Operation{}
	require.NoError(t, operationrun.UnmarshalConfig(raw, operation))

	return operation
}

func validCreateRequest() *pb.CreateOperationRunRequest {
	targetVersion := "1.2.3"

	return &pb.CreateOperationRunRequest{
		Name: "firmware canary",
		Configuration: &pb.OperationRunConfiguration{
			Selector: &pb.OperationRunSelector{
				Selector: &pb.OperationRunSelector_Percentage{
					Percentage: &pb.PercentageSelector{Percentage: 10},
				},
			},
			Options: &pb.OperationRunOptions{
				MaxConcurrentTargets: 2,
				SafetyPolicy: &pb.OperationRunSafetyPolicy{
					Gates: []*pb.OperationRunSafetyGate{
						{
							Gate: &pb.OperationRunSafetyGate_FailureRate{
								FailureRate: &pb.OperationRunFailureRateGate{
									FailureThresholdPercent: 20,
								},
							},
						},
					},
				},
			},
			Operation: &pb.OperationRunOperation{
				Operation: &pb.OperationRunOperation_UpgradeFirmware{
					UpgradeFirmware: &pb.UpgradeFirmwareRequest{
						TargetVersion: &targetVersion,
						Description:   "roll rack firmware",
						QueueOptions: &pb.QueueOptions{
							ConflictStrategy:    pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE,
							QueueTimeoutSeconds: 300,
						},
						RuleId: &pb.UUID{
							Id: "33333333-3333-3333-3333-333333333333",
						},
						SubTargets:             []string{"bmc"},
						OverrideReadinessCheck: true,
						OverrideVersionCheck:   true,
					},
				},
			},
		},
	}
}
