// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// OperationRunFrom converts a CreateOperationRunRequest to a
// domain operation run, validating request-owned fields and materializing
// server-owned defaults such as generated seeds and conflict retry durations.
// The returned OperationRun stores each normalized policy/configuration value
// as internal JSON for dispatcher use.
func OperationRunFrom(req *pb.CreateOperationRunRequest) (*operationrun.OperationRun, error) {
	return operationRunFrom(req, nil)
}

// OperationRunFromWithFirmwareAuthentication converts a request while adding
// already-encrypted firmware authentication data to the persisted operation
// template before its first serialization.
func OperationRunFromWithFirmwareAuthentication(
	req *pb.CreateOperationRunRequest,
	authenticationData *secret.EncryptedData,
) (*operationrun.OperationRun, error) {
	return operationRunFrom(req, authenticationData)
}

func operationRunFrom(
	req *pb.CreateOperationRunRequest,
	firmwareAuthenticationData *secret.EncryptedData,
) (*operationrun.OperationRun, error) {
	if req == nil {
		return nil, fmt.Errorf("create operation run request is required")
	}

	name := strings.TrimSpace(req.GetName())
	if name == "" {
		return nil, fmt.Errorf("name is required")
	}

	configuration := req.GetConfiguration()
	if configuration == nil {
		return nil, fmt.Errorf("configuration is required")
	}

	operation, err := operationFrom(configuration.GetOperation())
	if err != nil {
		return nil, err
	}
	if firmwareInfo, ok := operation.Payload.(*operations.FirmwareControlTaskInfo); ok {
		firmwareInfo.AuthenticationData = firmwareAuthenticationData
	}

	selector, err := selectorFrom(configuration.GetSelector())
	if err != nil {
		return nil, err
	}

	options, err := optionsFrom(
		configuration.GetOptions(),
		operation.Type,
		operation.Code,
	)
	if err != nil {
		return nil, err
	}

	selectorJSON, err := marshalConfig(selector)
	if err != nil {
		return nil, fmt.Errorf("marshal selector: %w", err)
	}

	optionsJSON, err := marshalConfig(options)
	if err != nil {
		return nil, fmt.Errorf("marshal options: %w", err)
	}

	operationJSON, err := marshalConfig(operation)
	if err != nil {
		return nil, fmt.Errorf("marshal operation template: %w", err)
	}

	return &operationrun.OperationRun{
		Name:              name,
		Description:       req.GetDescription(),
		Status:            operationrun.OperationRunStatusPending,
		StatusReason:      operationrun.OperationRunStatusReasonNone,
		Selector:          selectorJSON,
		Options:           optionsJSON,
		OperationTemplate: operationJSON,
		OperationType:     operation.Type,
		OperationCode:     operation.Code,
	}, nil
}

func selectorFrom(
	selector *pb.OperationRunSelector,
) (operationrun.Selector, error) {
	if selector.GetSelector() == nil {
		return nil, fmt.Errorf("selector is required")
	}

	switch s := selector.GetSelector().(type) {
	case *pb.OperationRunSelector_Percentage:
		if s.Percentage == nil {
			return nil, fmt.Errorf("percentage selector is required")
		}

		perc := s.Percentage.GetPercentage()
		if perc < 1 || perc > 100 {
			return nil, fmt.Errorf("percentage selector must be between 1 and 100")
		}

		seed := s.Percentage.GetSeed()
		if seed == "" {
			seed = uuid.NewString()
		}

		return &operationrun.PercentageSelector{
			Percentage: perc,
			Seed:       seed,
		}, nil
	default:
		return nil, fmt.Errorf("unsupported operation run selector")
	}
}

func optionsFrom(
	options *pb.OperationRunOptions,
	opType taskcommon.TaskType,
	opCode string,
) (*operationrun.Options, error) {
	if options == nil {
		return nil, fmt.Errorf("options are required")
	}
	if options.GetMaxConcurrentTargets() <= 0 {
		return nil, fmt.Errorf("options.max_concurrent_targets must be greater than 0")
	}

	safety, err := safetyPolicyFrom(options.GetSafetyPolicy())
	if err != nil {
		return nil, err
	}

	conflict, err := conflictPolicyFrom(
		options.GetConflictPolicy(),
		opType,
		opCode,
	)
	if err != nil {
		return nil, err
	}

	ordering, err := orderingPolicyFrom(options.GetOrderingPolicy())
	if err != nil {
		return nil, err
	}

	phase, err := phasePolicyFrom(options.GetPhasePolicy())
	if err != nil {
		return nil, err
	}

	return &operationrun.Options{
		MaxConcurrentTargets: options.GetMaxConcurrentTargets(),
		SafetyPolicy:         *safety,
		ConflictPolicy:       conflict,
		OrderingPolicy:       ordering,
		PhasePolicy:          *phase,
	}, nil
}

func safetyPolicyFrom(
	policy *pb.OperationRunSafetyPolicy,
) (*operationrun.SafetyPolicy, error) {
	if policy == nil {
		return nil, fmt.Errorf("options.safety_policy is required")
	}
	if len(policy.GetGates()) == 0 {
		return nil, fmt.Errorf("safety_policy must contain at least one gate")
	}

	gates := make([]operationrun.SafetyGate, 0, len(policy.GetGates()))
	for idx, gate := range policy.GetGates() {
		normalized, err := safetyGateFrom(gate)
		if err != nil {
			return nil, fmt.Errorf("safety_policy.gates[%d]: %w", idx, err)
		}
		gates = append(gates, normalized)
	}

	return &operationrun.SafetyPolicy{Gates: gates}, nil
}

func safetyGateFrom(
	gate *pb.OperationRunSafetyGate,
) (operationrun.SafetyGate, error) {
	switch g := gate.GetGate().(type) {
	case *pb.OperationRunSafetyGate_FailureRate:
		if g.FailureRate == nil {
			return nil, fmt.Errorf("failure_rate safety gate is required")
		}

		scope, err := safetyGateScopeFrom(
			g.FailureRate.GetScope(),
			"failure_rate.scope",
		)
		if err != nil {
			return nil, err
		}
		if g.FailureRate.GetFailureThresholdPercent() < 1 ||
			g.FailureRate.GetFailureThresholdPercent() > 100 {
			return nil, fmt.Errorf("failure_rate.failure_threshold_percent must be between 1 and 100")
		}

		return &operationrun.FailureRateGate{
			Scope:                   scope,
			FailureThresholdPercent: g.FailureRate.GetFailureThresholdPercent(),
		}, nil
	case *pb.OperationRunSafetyGate_FailureCount:
		if g.FailureCount == nil {
			return nil, fmt.Errorf("failure_count safety gate is required")
		}

		scope, err := safetyGateScopeFrom(
			g.FailureCount.GetScope(),
			"failure_count.scope",
		)
		if err != nil {
			return nil, err
		}
		if g.FailureCount.GetFailureThresholdCount() <= 0 {
			return nil, fmt.Errorf("failure_count.failure_threshold_count must be greater than 0")
		}

		return &operationrun.FailureCountGate{
			Scope:                 scope,
			FailureThresholdCount: g.FailureCount.GetFailureThresholdCount(),
		}, nil
	default:
		return nil, fmt.Errorf("unsupported safety gate")
	}
}

func safetyGateScopeFrom(
	scope pb.OperationRunSafetyGateScope,
	name string,
) (operationrun.SafetyGateScope, error) {
	switch scope {
	case pb.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_UNKNOWN:
		return operationrun.SafetyGateScopeCurrentPhase, nil
	case pb.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CURRENT_PHASE:
		return operationrun.SafetyGateScopeCurrentPhase, nil
	case pb.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CUMULATIVE_RUN:
		return operationrun.SafetyGateScopeCumulativeRun, nil
	default:
		return "", fmt.Errorf("%s is unsupported", name)
	}
}

func orderingPolicyFrom(
	policy *pb.OperationRunOrderingPolicy,
) (operationrun.OrderingPolicy, error) {
	if policy.GetOrdering() == nil {
		return defaultRandomOrdering(), nil
	}

	switch ordering := policy.GetOrdering().(type) {
	case *pb.OperationRunOrderingPolicy_Random:
		if ordering.Random == nil {
			return operationrun.OrderingPolicy{}, fmt.Errorf("random ordering policy is required")
		}

		seed := ordering.Random.GetSeed()
		if seed == "" {
			seed = uuid.NewString()
		}
		return operationrun.OrderingPolicy{
			Payload: &operationrun.RandomOrdering{Seed: seed},
		}, nil
	case *pb.OperationRunOrderingPolicy_PhysicalLocation:
		return operationrun.OrderingPolicy{}, fmt.Errorf("physical_location ordering is not supported yet")
	default:
		return operationrun.OrderingPolicy{}, fmt.Errorf("unsupported ordering policy")
	}
}

func defaultRandomOrdering() operationrun.OrderingPolicy {
	return operationrun.OrderingPolicy{
		Payload: &operationrun.RandomOrdering{Seed: uuid.NewString()},
	}
}

func phasePolicyFrom(
	policy *pb.OperationRunPhasePolicy,
) (*operationrun.PhasePolicy, error) {
	advance, err := phaseAdvancePolicyFrom(policy.GetAdvancePolicy())
	if err != nil {
		return nil, err
	}

	if policy.GetPlan() == nil {
		return &operationrun.PhasePolicy{
			Plan:          &operationrun.EqualPhases{PhaseCount: 1},
			AdvancePolicy: *advance,
		}, nil
	}

	switch plan := policy.GetPlan().(type) {
	case *pb.OperationRunPhasePolicy_Equal:
		if plan.Equal == nil {
			return nil, fmt.Errorf("equal phase policy is required")
		}
		if plan.Equal.GetPhaseCount() <= 0 {
			return nil, fmt.Errorf("equal phase_count must be greater than 0")
		}

		return &operationrun.PhasePolicy{
			Plan:          &operationrun.EqualPhases{PhaseCount: plan.Equal.GetPhaseCount()},
			AdvancePolicy: *advance,
		}, nil
	case *pb.OperationRunPhasePolicy_Percentage:
		percentage, err := percentagePhasesFrom(plan.Percentage)
		if err != nil {
			return nil, err
		}

		return &operationrun.PhasePolicy{
			Plan:          percentage,
			AdvancePolicy: *advance,
		}, nil
	case *pb.OperationRunPhasePolicy_Count:
		count, err := countPhasesFrom(plan.Count)
		if err != nil {
			return nil, err
		}

		return &operationrun.PhasePolicy{
			Plan:          count,
			AdvancePolicy: *advance,
		}, nil
	default:
		return nil, fmt.Errorf("unsupported phase policy")
	}
}

func percentagePhasesFrom(
	percentage *pb.PercentageOperationRunPhases,
) (*operationrun.PercentagePhases, error) {
	if len(percentage.GetPhases()) == 0 {
		return nil, fmt.Errorf("percentage phase policy must include at least one phase")
	}

	phases := make([]operationrun.PercentagePhase, 0, len(percentage.GetPhases()))
	sum := int32(0)
	for _, phase := range percentage.GetPhases() {
		if phase.GetPercentage() < 1 || phase.GetPercentage() > 100 {
			return nil, fmt.Errorf("percentage phase percentages must be between 1 and 100")
		}
		sum += phase.GetPercentage()
		phases = append(
			phases,
			operationrun.PercentagePhase{Percentage: phase.GetPercentage()},
		)
	}
	if sum != 100 {
		return nil, fmt.Errorf("percentage phase percentages must sum to 100")
	}

	return &operationrun.PercentagePhases{Phases: phases}, nil
}

func countPhasesFrom(
	count *pb.CountOperationRunPhases,
) (*operationrun.CountPhases, error) {
	if len(count.GetPhases()) == 0 {
		return nil, fmt.Errorf("count phase policy must include at least one phase")
	}

	phases := make([]operationrun.CountPhase, 0, len(count.GetPhases()))
	for _, phase := range count.GetPhases() {
		if phase.GetCount() <= 0 {
			return nil, fmt.Errorf("count phase counts must be greater than 0")
		}
		phases = append(
			phases,
			operationrun.CountPhase{Count: phase.GetCount()},
		)
	}

	return &operationrun.CountPhases{Phases: phases}, nil
}

func phaseAdvancePolicyFrom(
	policy *pb.OperationRunPhaseAdvancePolicy,
) (*operationrun.PhaseAdvancePolicy, error) {
	if policy == nil {
		return &operationrun.PhaseAdvancePolicy{}, nil
	}

	return &operationrun.PhaseAdvancePolicy{
		AutoAdvance: policy.GetAutoAdvance(),
	}, nil
}

func conflictPolicyFrom(
	policy *pb.OperationRunConflictPolicy,
	opType taskcommon.TaskType,
	opCode string,
) (operationrun.ConflictPolicy, error) {
	if policy.GetStrategy() == nil {
		return retryConflictPolicyFrom(nil, opType, opCode)
	}

	switch strategy := policy.GetStrategy().(type) {
	case *pb.OperationRunConflictPolicy_Retry:
		return retryConflictPolicyFrom(strategy.Retry, opType, opCode)
	default:
		return operationrun.ConflictPolicy{}, fmt.Errorf("unsupported conflict policy")
	}
}

func retryConflictPolicyFrom(
	pbRetry *pb.OperationRunConflictRetryPolicy,
	opType taskcommon.TaskType,
	opCode string,
) (operationrun.ConflictPolicy, error) {
	retry := conflictRetryDefaultsFor(opType, opCode)
	if pbRetry != nil {
		if pbRetry.RetryTimeout != nil {
			if err := pbRetry.RetryTimeout.CheckValid(); err != nil {
				return operationrun.ConflictPolicy{}, fmt.Errorf("conflict_policy.retry.retry_timeout is invalid: %w", err)
			}
			retry.RetryTimeout = pbRetry.RetryTimeout.AsDuration()
		}
		if pbRetry.InitialRetryDelay != nil {
			if err := pbRetry.InitialRetryDelay.CheckValid(); err != nil {
				return operationrun.ConflictPolicy{}, fmt.Errorf("conflict_policy.retry.initial_retry_delay is invalid: %w", err)
			}
			retry.InitialRetryDelay = pbRetry.InitialRetryDelay.AsDuration()
		}
		if pbRetry.MaxRetryDelay != nil {
			if err := pbRetry.MaxRetryDelay.CheckValid(); err != nil {
				return operationrun.ConflictPolicy{}, fmt.Errorf("conflict_policy.retry.max_retry_delay is invalid: %w", err)
			}
			retry.MaxRetryDelay = pbRetry.MaxRetryDelay.AsDuration()
		}
	}

	if err := retry.Validate(); err != nil {
		return operationrun.ConflictPolicy{}, fmt.Errorf("conflict_policy.retry: %w", err)
	}

	return operationrun.ConflictPolicy{Payload: &retry}, nil
}

func conflictRetryDefaultsFor(
	opType taskcommon.TaskType,
	opCode string,
) operationrun.ConflictRetryPolicy {
	// TODO: move these values into operation-specific configuration once the
	// first dispatcher implementation gives us production signal.
	switch {
	case opType == taskcommon.TaskTypeFirmwareControl &&
		opCode == taskcommon.OpCodeFirmwareControlUpgrade:
		return operationrun.ConflictRetryPolicy{
			RetryTimeout:      time.Hour,
			InitialRetryDelay: 30 * time.Second,
			MaxRetryDelay:     5 * time.Minute,
		}
	default:
		return operationrun.ConflictRetryPolicy{
			RetryTimeout:      time.Hour,
			InitialRetryDelay: 30 * time.Second,
			MaxRetryDelay:     5 * time.Minute,
		}
	}
}

func operationFrom(
	runOp *pb.OperationRunOperation,
) (*operationrun.Operation, error) {
	if runOp.GetOperation() == nil {
		return nil, fmt.Errorf("operation is required")
	}

	targetScope, err := operationTargetScopeFrom(runOp.GetTargetScope())
	if err != nil {
		return nil, err
	}

	var runOperation *operationrun.Operation

	switch op := runOp.GetOperation().(type) {
	case *pb.OperationRunOperation_UpgradeFirmware:
		runOperation, err = upgradeFirmwareOperationFrom(
			op.UpgradeFirmware,
		)
	default:
		return nil, fmt.Errorf("unsupported operation")
	}
	if err != nil {
		return nil, err
	}

	runOperation.TargetScope = *targetScope
	if err := runOperation.Validate(); err != nil {
		return nil, fmt.Errorf("validate operation: %w", err)
	}
	return runOperation, nil
}

func upgradeFirmwareOperationFrom(
	upgrade *pb.UpgradeFirmwareRequest,
) (*operationrun.Operation, error) {
	if upgrade == nil {
		return nil, fmt.Errorf("upgrade_firmware operation is required")
	}
	ruleID, err := OptionalUUIDFrom(upgrade.GetRuleId())
	if err != nil {
		return nil, fmt.Errorf("invalid upgrade_firmware.rule_id: %w", err)
	}

	var targetSpec *operation.TargetSpec
	if upgrade.GetTargetSpec() != nil {
		converted, err := TargetSpecFrom(upgrade.GetTargetSpec())
		if err != nil {
			return nil, fmt.Errorf(
				"invalid upgrade_firmware.target_spec: %w", err,
			)
		}
		targetSpec = &converted
	}

	info := &operations.FirmwareControlTaskInfo{
		Operation:              operations.FirmwareOperationUpgrade,
		TargetVersion:          upgrade.GetTargetVersion(),
		SubTargets:             append([]string(nil), upgrade.GetSubTargets()...),
		OverrideReadinessCheck: upgrade.GetOverrideReadinessCheck(),
	}
	if ruleID != nil {
		info.RuleID = ruleID.String()
	}
	if upgrade.GetStartTime() != nil {
		info.StartTime = upgrade.GetStartTime().AsTime().Unix()
	}
	if upgrade.GetEndTime() != nil {
		info.EndTime = upgrade.GetEndTime().AsTime().Unix()
	}

	return &operationrun.Operation{
		Type:         info.Type(),
		Code:         info.CodeString(),
		TargetSpec:   targetSpec,
		Description:  upgrade.GetDescription(),
		QueueOptions: queueOptionsFrom(upgrade.GetQueueOptions()),
		Payload:      info,
	}, nil
}

// operationTargetScopeFrom converts prior-run exclusions into the internal
// operation-run scope model. The embedded operation target_spec, when present,
// is always the inclusive base scope.
func operationTargetScopeFrom(
	scope *pb.OperationRunTargetScope,
) (*operationrun.OperationTargetScope, error) {
	if scope == nil {
		return &operationrun.OperationTargetScope{}, nil
	}

	runIDs := scope.GetExcludeOperationRunIds()
	normalizedRunIDs := make([]uuid.UUID, 0, len(runIDs))
	for i, id := range runIDs {
		parsed, err := uuid.Parse(id.GetId())
		if err != nil || parsed == uuid.Nil {
			return nil, fmt.Errorf(
				"target_scope.exclude_operation_run_ids[%d] must be a valid UUID",
				i,
			)
		}

		normalizedRunIDs = append(normalizedRunIDs, parsed)
	}

	filter, err := defaultScopeComponentFilterFrom(scope)
	if err != nil {
		return nil, err
	}

	return &operationrun.OperationTargetScope{
		ExcludedOperationRunIDs:     normalizedRunIDs,
		DefaultScopeComponentFilter: filter,
	}, nil
}

func defaultScopeComponentFilterFrom(
	scope *pb.OperationRunTargetScope,
) (*operation.ComponentFilter, error) {
	filter := scope.GetDefaultScopeComponentFilter()
	if filter == nil {
		return nil, nil
	}

	switch payload := filter.GetFilter().(type) {
	case nil:
		return nil, nil
	case *pb.ComponentFilter_Types:
		componentTypes := payload.Types.GetTypes()
		types := make([]string, 0, len(componentTypes))
		for idx, pbType := range componentTypes {
			componentType := ComponentTypeFrom(pbType)
			if componentType == devicetypes.ComponentTypeUnknown {
				return nil, fmt.Errorf(
					"target_scope.default_scope_component_filter.types[%d] is unknown",
					idx,
				)
			}
			types = append(types, devicetypes.ComponentTypeToString(componentType))
		}
		return &operation.ComponentFilter{
			Kind:  operation.ComponentFilterKindTypes,
			Types: types,
		}, nil
	case *pb.ComponentFilter_Components:
		componentTargets := payload.Components.GetTargets()
		components := make([]uuid.UUID, 0, len(componentTargets))
		for idx, target := range componentTargets {
			idTarget, ok := target.GetIdentifier().(*pb.ComponentTarget_Id)
			if !ok {
				return nil, fmt.Errorf(
					"target_scope.default_scope_component_filter.components[%d] must use component UUID",
					idx,
				)
			}
			parsed, err := uuid.Parse(idTarget.Id.GetId())
			if err != nil || parsed == uuid.Nil {
				return nil, fmt.Errorf(
					"target_scope.default_scope_component_filter.components[%d] must be a valid UUID",
					idx,
				)
			}
			components = append(components, parsed)
		}
		return &operation.ComponentFilter{
			Kind:       operation.ComponentFilterKindComponents,
			Components: components,
		}, nil
	default:
		return nil, fmt.Errorf("unsupported target_scope default scope component filter")
	}
}

func marshalConfig(value any) (json.RawMessage, error) {
	if value == nil {
		return nil, fmt.Errorf("operation-run config value is nil")
	}

	return operationrun.MarshalConfig(value)
}

func selectorTo(selector operationrun.Selector) (*pb.OperationRunSelector, error) {
	if selector == nil {
		return nil, fmt.Errorf("selector is required")
	}

	switch selector.SelectorKind() {
	case operationrun.SelectorKindPercentage:
		percentage, ok := selector.(*operationrun.PercentageSelector)
		if !ok || percentage == nil {
			return nil, fmt.Errorf("percentage selector is required")
		}
		return &pb.OperationRunSelector{
			Selector: &pb.OperationRunSelector_Percentage{
				Percentage: &pb.PercentageSelector{
					Percentage: percentage.Percentage,
					Seed:       percentage.Seed,
				},
			},
		}, nil
	default:
		return nil, fmt.Errorf("unsupported selector kind %q", selector.SelectorKind())
	}
}

func optionsTo(options *operationrun.Options) (*pb.OperationRunOptions, error) {
	if options == nil {
		return nil, fmt.Errorf("options are required")
	}

	safety, err := safetyPolicyTo(&options.SafetyPolicy)
	if err != nil {
		return nil, err
	}
	conflict, err := conflictPolicyTo(options.ConflictPolicy)
	if err != nil {
		return nil, err
	}
	ordering, err := orderingPolicyTo(options.OrderingPolicy)
	if err != nil {
		return nil, err
	}
	phase, err := phasePolicyTo(&options.PhasePolicy)
	if err != nil {
		return nil, err
	}

	return &pb.OperationRunOptions{
		MaxConcurrentTargets: options.MaxConcurrentTargets,
		SafetyPolicy:         safety,
		ConflictPolicy:       conflict,
		OrderingPolicy:       ordering,
		PhasePolicy:          phase,
	}, nil
}

func safetyPolicyTo(
	policy *operationrun.SafetyPolicy,
) (*pb.OperationRunSafetyPolicy, error) {
	if policy == nil {
		return nil, fmt.Errorf("safety policy is required")
	}

	gates := make([]*pb.OperationRunSafetyGate, 0, len(policy.Gates))
	for idx := range policy.Gates {
		gate, err := safetyGateTo(policy.Gates[idx])
		if err != nil {
			return nil, fmt.Errorf("safety_policy.gates[%d]: %w", idx, err)
		}
		gates = append(gates, gate)
	}

	return &pb.OperationRunSafetyPolicy{Gates: gates}, nil
}

func safetyGateTo(
	gate operationrun.SafetyGate,
) (*pb.OperationRunSafetyGate, error) {
	if gate == nil {
		return nil, fmt.Errorf("safety gate is required")
	}

	switch gate.SafetyGateKind() {
	case operationrun.SafetyGateKindFailureRate:
		failureRate, ok := gate.(*operationrun.FailureRateGate)
		if !ok || failureRate == nil {
			return nil, fmt.Errorf("failure_rate safety gate is required")
		}
		return &pb.OperationRunSafetyGate{
			Gate: &pb.OperationRunSafetyGate_FailureRate{
				FailureRate: &pb.OperationRunFailureRateGate{
					Scope:                   safetyGateScopeTo(failureRate.Scope),
					FailureThresholdPercent: failureRate.FailureThresholdPercent,
				},
			},
		}, nil
	case operationrun.SafetyGateKindFailureCount:
		failureCount, ok := gate.(*operationrun.FailureCountGate)
		if !ok || failureCount == nil {
			return nil, fmt.Errorf("failure_count safety gate is required")
		}
		return &pb.OperationRunSafetyGate{
			Gate: &pb.OperationRunSafetyGate_FailureCount{
				FailureCount: &pb.OperationRunFailureCountGate{
					Scope:                 safetyGateScopeTo(failureCount.Scope),
					FailureThresholdCount: failureCount.FailureThresholdCount,
				},
			},
		}, nil
	default:
		return nil, fmt.Errorf("unsupported safety gate kind %q", gate.SafetyGateKind())
	}
}

func safetyGateScopeTo(
	scope operationrun.SafetyGateScope,
) pb.OperationRunSafetyGateScope {
	switch scope {
	case operationrun.SafetyGateScopeCumulativeRun:
		return pb.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CUMULATIVE_RUN
	default:
		return pb.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CURRENT_PHASE
	}
}

func conflictPolicyTo(
	policy operationrun.ConflictPolicy,
) (*pb.OperationRunConflictPolicy, error) {
	if policy.Payload == nil {
		return nil, fmt.Errorf("conflict policy is required")
	}

	switch policy.Payload.ConflictPolicyKind() {
	case operationrun.ConflictPolicyKindRetry:
		retry, ok := policy.Payload.(*operationrun.ConflictRetryPolicy)
		if !ok || retry == nil {
			return nil, fmt.Errorf("retry conflict policy is required")
		}
		return &pb.OperationRunConflictPolicy{
			Strategy: &pb.OperationRunConflictPolicy_Retry{
				Retry: &pb.OperationRunConflictRetryPolicy{
					RetryTimeout:      durationpb.New(retry.RetryTimeout),
					InitialRetryDelay: durationpb.New(retry.InitialRetryDelay),
					MaxRetryDelay:     durationpb.New(retry.MaxRetryDelay),
				},
			},
		}, nil
	default:
		return nil, fmt.Errorf("unsupported conflict policy kind %q", policy.Payload.ConflictPolicyKind())
	}
}

func orderingPolicyTo(
	policy operationrun.OrderingPolicy,
) (*pb.OperationRunOrderingPolicy, error) {
	if policy.Payload == nil {
		return nil, fmt.Errorf("ordering policy is required")
	}

	switch policy.Payload.OrderingPolicyKind() {
	case operationrun.OrderingPolicyKindRandom:
		random, ok := policy.Payload.(*operationrun.RandomOrdering)
		if !ok || random == nil {
			return nil, fmt.Errorf("random ordering policy is required")
		}
		return &pb.OperationRunOrderingPolicy{
			Ordering: &pb.OperationRunOrderingPolicy_Random{
				Random: &pb.OperationRunRandomOrdering{Seed: random.Seed},
			},
		}, nil
	case operationrun.OrderingPolicyKindPhysicalLocation:
		physicalLocation, ok := policy.Payload.(*operationrun.PhysicalLocationOrdering)
		if !ok || physicalLocation == nil {
			return nil, fmt.Errorf("physical_location ordering policy is required")
		}
		return &pb.OperationRunOrderingPolicy{
			Ordering: &pb.OperationRunOrderingPolicy_PhysicalLocation{
				PhysicalLocation: &pb.OperationRunPhysicalLocationOrdering{
					Strategy: physicalLocationOrderingStrategyTo(
						physicalLocation.Strategy,
					),
				},
			},
		}, nil
	default:
		return nil, fmt.Errorf("unsupported ordering policy kind %q", policy.Payload.OrderingPolicyKind())
	}
}

func physicalLocationOrderingStrategyTo(
	strategy operationrun.PhysicalLocationOrderingStrategy,
) pb.OperationRunPhysicalLocationOrdering_Strategy {
	switch strategy {
	case operationrun.PhysicalLocationOrderingStrategyOnePerRowRoundRobin:
		return pb.OperationRunPhysicalLocationOrdering_STRATEGY_ONE_PER_ROW_ROUND_ROBIN
	case operationrun.PhysicalLocationOrderingStrategyRowByRow:
		return pb.OperationRunPhysicalLocationOrdering_STRATEGY_ROW_BY_ROW
	default:
		return pb.OperationRunPhysicalLocationOrdering_STRATEGY_UNKNOWN
	}
}

func phasePolicyTo(
	policy *operationrun.PhasePolicy,
) (*pb.OperationRunPhasePolicy, error) {
	if policy == nil {
		return nil, fmt.Errorf("phase policy is required")
	}
	if policy.Plan == nil {
		return nil, fmt.Errorf("phase policy plan is required")
	}

	result := &pb.OperationRunPhasePolicy{
		AdvancePolicy: &pb.OperationRunPhaseAdvancePolicy{
			AutoAdvance: policy.AdvancePolicy.AutoAdvance,
		},
	}

	switch policy.Plan.PhasePlanKind() {
	case operationrun.PhasePlanKindEqual:
		equal, ok := policy.Plan.(*operationrun.EqualPhases)
		if !ok || equal == nil {
			return nil, fmt.Errorf("equal phase policy is required")
		}
		result.Plan = &pb.OperationRunPhasePolicy_Equal{
			Equal: &pb.EqualOperationRunPhases{
				PhaseCount: equal.PhaseCount,
			},
		}
	case operationrun.PhasePlanKindPercentage:
		percentage, ok := policy.Plan.(*operationrun.PercentagePhases)
		if !ok || percentage == nil {
			return nil, fmt.Errorf("percentage phase policy is required")
		}
		phases := make(
			[]*pb.OperationRunPercentagePhase,
			0,
			len(percentage.Phases),
		)
		for _, phase := range percentage.Phases {
			phases = append(phases, &pb.OperationRunPercentagePhase{
				Percentage: phase.Percentage,
			})
		}
		result.Plan = &pb.OperationRunPhasePolicy_Percentage{
			Percentage: &pb.PercentageOperationRunPhases{Phases: phases},
		}
	case operationrun.PhasePlanKindCount:
		count, ok := policy.Plan.(*operationrun.CountPhases)
		if !ok || count == nil {
			return nil, fmt.Errorf("count phase policy is required")
		}
		phases := make(
			[]*pb.OperationRunCountPhase,
			0,
			len(count.Phases),
		)
		for _, phase := range count.Phases {
			phases = append(phases, &pb.OperationRunCountPhase{
				Count: phase.Count,
			})
		}
		result.Plan = &pb.OperationRunPhasePolicy_Count{
			Count: &pb.CountOperationRunPhases{Phases: phases},
		}
	default:
		return nil, fmt.Errorf("unsupported phase policy kind %q", policy.Plan.PhasePlanKind())
	}

	return result, nil
}

func operationTo(
	runOperation *operationrun.Operation,
) (*pb.OperationRunOperation, error) {
	if runOperation == nil {
		return nil, fmt.Errorf("operation is required")
	}

	switch {
	case runOperation.Type == taskcommon.TaskTypeFirmwareControl &&
		runOperation.Code == taskcommon.OpCodeFirmwareControlUpgrade:
		info, err := firmwareControlTaskInfoFrom(runOperation)
		if err != nil {
			return nil, err
		}

		upgradeFirmware := &pb.UpgradeFirmwareRequest{
			Description:            runOperation.Description,
			QueueOptions:           queueOptionsTo(runOperation.QueueOptions),
			RuleId:                 optionalUUIDStringTo(info.RuleID),
			SubTargets:             append([]string(nil), info.SubTargets...),
			OverrideReadinessCheck: info.OverrideReadinessCheck,
		}
		if info.TargetVersion != "" {
			targetVersion := info.TargetVersion
			upgradeFirmware.TargetVersion = &targetVersion
		}
		if runOperation.TargetSpec != nil {
			targetSpec, err := TargetSpecTo(*runOperation.TargetSpec)
			if err != nil {
				return nil, fmt.Errorf("convert target_spec: %w", err)
			}
			upgradeFirmware.TargetSpec = targetSpec
		}
		if info.StartTime != 0 {
			upgradeFirmware.StartTime = timestamppb.New(
				time.Unix(info.StartTime, 0),
			)
		}
		if info.EndTime != 0 {
			upgradeFirmware.EndTime = timestamppb.New(
				time.Unix(info.EndTime, 0),
			)
		}

		return &pb.OperationRunOperation{
			Operation: &pb.OperationRunOperation_UpgradeFirmware{
				UpgradeFirmware: upgradeFirmware,
			},
			TargetScope: targetScopeTo(&runOperation.TargetScope),
		}, nil
	default:
		return nil, fmt.Errorf(
			"unsupported operation kind %q/%q",
			runOperation.Type,
			runOperation.Code,
		)
	}
}

func firmwareControlTaskInfoFrom(
	runOperation *operationrun.Operation,
) (*operations.FirmwareControlTaskInfo, error) {
	info, ok := runOperation.Payload.(*operations.FirmwareControlTaskInfo)
	if !ok || info == nil {
		return nil, fmt.Errorf("firmware_control operation payload is required")
	}
	if info.Operation != operations.FirmwareOperationUpgrade {
		return nil, fmt.Errorf("unsupported firmware operation %q", info.Operation)
	}

	return info, nil
}

func queueOptionsFrom(opts *pb.QueueOptions) *operationrun.QueueOptions {
	if opts == nil {
		return nil
	}

	return &operationrun.QueueOptions{
		ConflictStrategy:    conflictStrategyFrom(opts.GetConflictStrategy()),
		QueueTimeoutSeconds: opts.GetQueueTimeoutSeconds(),
	}
}

func queueOptionsTo(opts *operationrun.QueueOptions) *pb.QueueOptions {
	if opts == nil {
		return nil
	}

	return &pb.QueueOptions{
		ConflictStrategy:    conflictStrategyTo(opts.ConflictStrategy),
		QueueTimeoutSeconds: opts.QueueTimeoutSeconds,
	}
}

func conflictStrategyFrom(strategy pb.ConflictStrategy) operation.ConflictStrategy {
	switch strategy {
	case pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE:
		return operation.ConflictStrategyQueue
	default:
		return operation.ConflictStrategyReject
	}
}

func conflictStrategyTo(strategy operation.ConflictStrategy) pb.ConflictStrategy {
	switch strategy {
	case operation.ConflictStrategyQueue:
		return pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE
	default:
		return pb.ConflictStrategy_CONFLICT_STRATEGY_REJECT
	}
}

func optionalUUIDStringTo(id string) *pb.UUID {
	parsed, err := uuid.Parse(id)
	if err != nil || parsed == uuid.Nil {
		return nil
	}

	return UUIDTo(parsed)
}

func targetScopeTo(scope *operationrun.OperationTargetScope) *pb.OperationRunTargetScope {
	if scope == nil {
		return nil
	}

	result := &pb.OperationRunTargetScope{
		ExcludeOperationRunIds: UUIDsTo(scope.ExcludedOperationRunIDs),
	}

	if scope.DefaultScopeComponentFilter != nil {
		switch scope.DefaultScopeComponentFilter.Kind {
		case operation.ComponentFilterKindTypes:
			types := make([]pb.ComponentType, 0, len(scope.DefaultScopeComponentFilter.Types))
			for _, rawType := range scope.DefaultScopeComponentFilter.Types {
				types = append(
					types,
					ComponentTypeTo(devicetypes.ComponentTypeFromString(rawType)),
				)
			}
			result.DefaultScopeComponentFilter = &pb.ComponentFilter{
				Filter: &pb.ComponentFilter_Types{
					Types: &pb.ComponentTypes{Types: types},
				},
			}
		case operation.ComponentFilterKindComponents:
			components := make(
				[]*pb.ComponentTarget,
				0,
				len(scope.DefaultScopeComponentFilter.Components),
			)
			for _, id := range scope.DefaultScopeComponentFilter.Components {
				components = append(components, &pb.ComponentTarget{
					Identifier: &pb.ComponentTarget_Id{Id: UUIDTo(id)},
				})
			}
			result.DefaultScopeComponentFilter = &pb.ComponentFilter{
				Filter: &pb.ComponentFilter_Components{
					Components: &pb.ComponentTargets{Targets: components},
				},
			}
		}
	}

	return result
}

// OperationRunTo converts a domain operation run to its API shape.
func OperationRunTo(run *operationrun.OperationRun) (*pb.OperationRun, error) {
	if run == nil {
		return nil, nil
	}

	var selector operationrun.Selector
	if err := operationrun.UnmarshalConfig(run.Selector, &selector); err != nil {
		return nil, fmt.Errorf("unmarshal selector: %w", err)
	}

	var options operationrun.Options
	if err := operationrun.UnmarshalConfig(run.Options, &options); err != nil {
		return nil, fmt.Errorf("unmarshal options: %w", err)
	}

	var operation operationrun.Operation
	if err := operationrun.UnmarshalConfig(run.OperationTemplate, &operation); err != nil {
		return nil, fmt.Errorf("unmarshal operation template: %w", err)
	}

	pbSelector, err := selectorTo(selector)
	if err != nil {
		return nil, fmt.Errorf("convert selector: %w", err)
	}

	pbOptions, err := optionsTo(&options)
	if err != nil {
		return nil, fmt.Errorf("convert options: %w", err)
	}

	pbOperation, err := operationTo(&operation)
	if err != nil {
		return nil, fmt.Errorf("convert operation template: %w", err)
	}

	result := &pb.OperationRun{
		Summary: operationRunSummaryTo(run),
		Configuration: &pb.OperationRunConfiguration{
			Selector:  pbSelector,
			Options:   pbOptions,
			Operation: pbOperation,
		},
	}
	// Stats are derived from operation_run_target rows, not persisted on
	// operation_run. Get handlers should set Stats only when include_stats asks
	// Flow to query those aggregates.

	return result, nil
}

// OperationRunSummaryTo converts a domain operation run to the lightweight
// list shape without unmarshalling configuration JSON.
func OperationRunSummaryTo(
	run *operationrun.OperationRun,
) (*pb.OperationRunSummary, error) {
	if run == nil {
		return nil, nil
	}

	return operationRunSummaryTo(run), nil
}

func operationRunSummaryTo(
	run *operationrun.OperationRun,
) *pb.OperationRunSummary {
	summary := &pb.OperationRunSummary{
		Id:            UUIDTo(run.ID),
		Name:          run.Name,
		Description:   run.Description,
		OperationKind: OperationKindTo(run.OperationType, run.OperationCode),
		State:         OperationRunStateTo(run.Status, run.StatusReason),
		StatusMessage: run.StatusMessage,
		TotalPhases:   run.TotalPhases,
		CreatedAt:     timestamppb.New(run.CreatedAt),
		UpdatedAt:     timestamppb.New(run.UpdatedAt),
	}

	if run.StartedAt != nil {
		summary.StartedAt = timestamppb.New(*run.StartedAt)
	}
	if run.FinishedAt != nil {
		summary.FinishedAt = timestamppb.New(*run.FinishedAt)
	}

	return summary
}

// OperationRunStateTo converts domain status fields to the API state
// message.
func OperationRunStateTo(
	status operationrun.OperationRunStatus,
	reason operationrun.OperationRunStatusReason,
) *pb.OperationRunState {
	return &pb.OperationRunState{
		Status: OperationRunStatusTo(status),
		Reason: OperationRunStatusReasonTo(reason),
	}
}

// OperationKindTo converts the operation type/code pair to its API shape.
func OperationKindTo(
	opType taskcommon.TaskType,
	opCode string,
) *pb.OperationKind {
	kind := &pb.OperationKind{Type: OperationTypeToProto(opType)}
	if opCode != "" {
		kind.Code = &opCode
	}

	return kind
}

// OperationRunTargetTo converts a domain operation-run target to its API
// shape.
func OperationRunTargetTo(
	target *operationrun.OperationRunTarget,
) (*pb.OperationRunTarget, error) {
	if target == nil {
		return nil, nil
	}

	result := &pb.OperationRunTarget{
		Id:               UUIDTo(target.ID),
		OperationRunId:   UUIDTo(target.OperationRunID),
		RackId:           UUIDTo(target.RackID),
		SequenceIndex:    target.SequenceIndex,
		PhaseIndex:       target.PhaseIndex,
		Status:           OperationRunTargetStatusTo(target.Status),
		Message:          target.Message,
		ComponentsByType: componentsByTypeTo(target.ComponentsByType),
		CreatedAt:        timestamppb.New(target.CreatedAt),
		UpdatedAt:        timestamppb.New(target.UpdatedAt),
	}

	if target.TaskID != nil {
		result.TaskId = UUIDTo(*target.TaskID)
	}

	return result, nil
}

// ProgressStatsTo converts domain operation-run progress stats to protobuf.
func ProgressStatsTo(
	stats operationrun.ProgressStats,
) *pb.OperationRunStats {
	return &pb.OperationRunStats{
		CurrentPhaseStats:    PhaseStatsTo(stats.CurrentPhase),
		CumulativePhaseStats: PhaseStatsTo(stats.Cumulative),
	}
}

// ProgressStatsFrom converts protobuf operation-run stats to the domain shape.
func ProgressStatsFrom(
	stats *pb.OperationRunStats,
) operationrun.ProgressStats {
	if stats == nil {
		return operationrun.ProgressStats{}
	}

	return operationrun.ProgressStats{
		CurrentPhase: PhaseStatsFrom(stats.GetCurrentPhaseStats()),
		Cumulative:   PhaseStatsFrom(stats.GetCumulativePhaseStats()),
	}
}

// PhaseStatsTo converts domain phase stats to protobuf.
func PhaseStatsTo(
	stats operationrun.PhaseStats,
) *pb.OperationRunPhaseStats {
	return &pb.OperationRunPhaseStats{
		PhaseIndex:      stats.PhaseIndex,
		SelectedTargets: int32(stats.SelectedTargets),
		OutcomeCounts:   TargetStatusCountsTo(stats.StatusCounts),
	}
}

// PhaseStatsFrom converts protobuf phase stats to the domain shape.
func PhaseStatsFrom(
	stats *pb.OperationRunPhaseStats,
) operationrun.PhaseStats {
	if stats == nil {
		return operationrun.PhaseStats{}
	}

	return operationrun.PhaseStats{
		PhaseIndex:      stats.GetPhaseIndex(),
		SelectedTargets: int(stats.GetSelectedTargets()),
		StatusCounts:    TargetStatusCountsFrom(stats.GetOutcomeCounts()),
	}
}

// TargetStatusCountsTo converts domain target status counts to protobuf
// outcome counts.
func TargetStatusCountsTo(
	stats operationrun.TargetStatusCounts,
) *pb.OperationRunTargetOutcomeCounts {
	return &pb.OperationRunTargetOutcomeCounts{
		Completed:  int32(stats.Completed),
		Failed:     int32(stats.Failed),
		Terminated: int32(stats.Terminated),
		Skipped:    int32(stats.Skipped),
	}
}

// TargetStatusCountsFrom converts protobuf target outcome counts to the domain
// status-count shape.
func TargetStatusCountsFrom(
	counts *pb.OperationRunTargetOutcomeCounts,
) operationrun.TargetStatusCounts {
	if counts == nil {
		return operationrun.TargetStatusCounts{}
	}

	return operationrun.TargetStatusCounts{
		Completed:  int(counts.GetCompleted()),
		Failed:     int(counts.GetFailed()),
		Terminated: int(counts.GetTerminated()),
		Skipped:    int(counts.GetSkipped()),
	}
}

// OperationRunListOptionsFrom converts a list-runs API request into domain
// list options.
func OperationRunListOptionsFrom(
	req *pb.ListOperationRunsRequest,
) (operationrun.ListOptions, error) {
	opts := operationrun.ListOptions{
		Pagination: PaginationFrom(nil),
	}

	if req == nil {
		return opts, nil
	}

	opts.Pagination = PaginationFrom(req.GetPagination())

	filter := req.GetFilter()
	if filter == nil {
		return opts, nil
	}

	opts.Name = StringQueryInfoFrom(filter.GetName())

	for _, state := range filter.GetStates() {
		if state == nil {
			continue
		}
		if state.Status == nil && state.Reason == nil {
			return operationrun.ListOptions{}, fmt.Errorf("operation run state filter must set status, reason, or both")
		}

		stateFilter := operationrun.StateFilter{}
		if state.Status != nil {
			stateFilter.Status = OperationRunStatusFrom(state.GetStatus())
			if stateFilter.Status == "" {
				return operationrun.ListOptions{}, fmt.Errorf("unknown operation run status in filter")
			}
		}
		if state.Reason != nil {
			stateFilter.Reason = OperationRunStatusReasonFrom(
				state.GetReason(),
			)
			if stateFilter.Reason == "" {
				return operationrun.ListOptions{}, fmt.Errorf("unknown operation run status reason in filter")
			}
		}

		opts.States = append(opts.States, stateFilter)
	}

	for _, kind := range filter.GetOperationKinds() {
		if kind == nil {
			continue
		}

		opType := OperationTypeFromProto(kind.GetType())
		if opType == taskcommon.TaskTypeUnknown {
			return operationrun.ListOptions{}, fmt.Errorf("operation kind filter must set operation type")
		}

		kindFilter := operationrun.OperationKindFilter{Type: opType}
		if kind.Code != nil {
			if kind.GetCode() == "" {
				return operationrun.ListOptions{}, fmt.Errorf("operation kind filter code must not be empty")
			}
			kindFilter.Code = kind.GetCode()
		}

		opts.OperationKinds = append(opts.OperationKinds, kindFilter)
	}

	return opts, nil
}

// OperationRunTargetListOptionsFrom converts a list-targets API request into
// domain list options.
func OperationRunTargetListOptionsFrom(
	req *pb.ListOperationRunTargetsRequest,
) (operationrun.TargetListOptions, error) {
	opts := operationrun.TargetListOptions{
		PhaseScope: operationrun.TargetPhaseScopeCurrentPhase,
		Pagination: PaginationFrom(nil),
	}

	if req == nil {
		return opts, nil
	}

	opts.Pagination = PaginationFrom(req.GetPagination())

	if req.GetStatus() != pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_UNKNOWN {
		opts.Status = OperationRunTargetStatusFrom(req.GetStatus())
		if opts.Status == "" {
			return operationrun.TargetListOptions{}, fmt.Errorf("unknown operation run target status in filter")
		}
	}

	switch req.GetPhaseScope() {
	case pb.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_UNKNOWN,
		pb.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_PHASE:
		opts.PhaseScope = operationrun.TargetPhaseScopeCurrentPhase
	case pb.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_COMPLETED_PHASES:
		opts.PhaseScope = operationrun.TargetPhaseScopeCompletedPhases
	case pb.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_AND_COMPLETED_PHASES:
		opts.PhaseScope = operationrun.TargetPhaseScopeCurrentAndCompletedPhases
	default:
		return operationrun.TargetListOptions{}, fmt.Errorf("unsupported operation run target phase scope")
	}

	return opts, nil
}

// OperationRunStatusTo converts a domain operation-run status to proto.
func OperationRunStatusTo(
	status operationrun.OperationRunStatus,
) pb.OperationRunStatus {
	switch status {
	case operationrun.OperationRunStatusPending:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_PENDING
	case operationrun.OperationRunStatusRunning:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_RUNNING
	case operationrun.OperationRunStatusPaused:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_PAUSED
	case operationrun.OperationRunStatusCompleted:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED
	case operationrun.OperationRunStatusCompletedWithFailures:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED_WITH_FAILURES
	case operationrun.OperationRunStatusCancelled:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_CANCELLED
	case operationrun.OperationRunStatusFailed:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_FAILED
	default:
		return pb.OperationRunStatus_OPERATION_RUN_STATUS_UNKNOWN
	}
}

// OperationRunStatusFrom converts an API status filter to the domain
// value.
// UNKNOWN returns the empty string, which callers treat as "no status filter".
func OperationRunStatusFrom(
	status pb.OperationRunStatus,
) operationrun.OperationRunStatus {
	switch status {
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_PENDING:
		return operationrun.OperationRunStatusPending
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_RUNNING:
		return operationrun.OperationRunStatusRunning
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_PAUSED:
		return operationrun.OperationRunStatusPaused
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED:
		return operationrun.OperationRunStatusCompleted
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED_WITH_FAILURES:
		return operationrun.OperationRunStatusCompletedWithFailures
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_CANCELLED:
		return operationrun.OperationRunStatusCancelled
	case pb.OperationRunStatus_OPERATION_RUN_STATUS_FAILED:
		return operationrun.OperationRunStatusFailed
	default:
		return ""
	}
}

// OperationRunStatusReasonTo converts a domain operation-run status
// reason to proto.
func OperationRunStatusReasonTo(
	reason operationrun.OperationRunStatusReason,
) pb.OperationRunStatusReason {
	switch reason {
	case operationrun.OperationRunStatusReasonNone:
		return pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_NONE
	case operationrun.OperationRunStatusReasonOperatorPaused:
		return pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_OPERATOR_PAUSED
	case operationrun.OperationRunStatusReasonPhaseGate:
		return pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_PHASE_GATE
	case operationrun.OperationRunStatusReasonSafetyGate:
		return pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_SAFETY_GATE
	case operationrun.OperationRunStatusReasonConflictRetryTimeout:
		return pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_CONFLICT_RETRY_TIMEOUT
	default:
		return pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_UNKNOWN
	}
}

// OperationRunStatusReasonFrom converts an API status reason to the
// domain value. UNKNOWN returns the empty string.
func OperationRunStatusReasonFrom(
	reason pb.OperationRunStatusReason,
) operationrun.OperationRunStatusReason {
	switch reason {
	case pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_NONE:
		return operationrun.OperationRunStatusReasonNone
	case pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_OPERATOR_PAUSED:
		return operationrun.OperationRunStatusReasonOperatorPaused
	case pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_PHASE_GATE:
		return operationrun.OperationRunStatusReasonPhaseGate
	case pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_SAFETY_GATE:
		return operationrun.OperationRunStatusReasonSafetyGate
	case pb.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_CONFLICT_RETRY_TIMEOUT:
		return operationrun.OperationRunStatusReasonConflictRetryTimeout
	default:
		return ""
	}
}

// OperationRunTargetStatusTo converts a domain target status to proto.
func OperationRunTargetStatusTo(
	status operationrun.OperationRunTargetStatus,
) pb.OperationRunTargetStatus {
	switch status {
	case operationrun.OperationRunTargetStatusPending:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_PENDING
	case operationrun.OperationRunTargetStatusClaimed:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_CLAIMED
	case operationrun.OperationRunTargetStatusBlocked:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_BLOCKED
	case operationrun.OperationRunTargetStatusSubmitted:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SUBMITTED
	case operationrun.OperationRunTargetStatusCompleted:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_COMPLETED
	case operationrun.OperationRunTargetStatusFailed:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_FAILED
	case operationrun.OperationRunTargetStatusTerminated:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_TERMINATED
	case operationrun.OperationRunTargetStatusSkipped:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SKIPPED
	default:
		return pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_UNKNOWN
	}
}

// OperationRunTargetStatusFrom converts an API target-status filter to
// the domain value. UNKNOWN returns the empty string, which callers treat as
// "no status filter".
func OperationRunTargetStatusFrom(
	status pb.OperationRunTargetStatus,
) operationrun.OperationRunTargetStatus {
	switch status {
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_PENDING:
		return operationrun.OperationRunTargetStatusPending
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_CLAIMED:
		return operationrun.OperationRunTargetStatusClaimed
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_BLOCKED:
		return operationrun.OperationRunTargetStatusBlocked
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SUBMITTED:
		return operationrun.OperationRunTargetStatusSubmitted
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_COMPLETED:
		return operationrun.OperationRunTargetStatusCompleted
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_FAILED:
		return operationrun.OperationRunTargetStatusFailed
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_TERMINATED:
		return operationrun.OperationRunTargetStatusTerminated
	case pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SKIPPED:
		return operationrun.OperationRunTargetStatusSkipped
	default:
		return ""
	}
}

func componentsByTypeTo(
	componentsByType operation.ComponentsByType,
) *pb.ComponentsByType {
	if len(componentsByType) == 0 {
		return nil
	}

	result := &pb.ComponentsByType{
		Groups: make([]*pb.ComponentsForType, 0, len(componentsByType)),
	}
	for _, componentType := range componentsByType.SortedComponentTypes() {
		ids := append([]uuid.UUID(nil), componentsByType[componentType]...)
		operation.SortComponentUUIDs(ids)
		group := &pb.ComponentsForType{
			Type:         ComponentTypeTo(componentType),
			ComponentIds: make([]*pb.UUID, 0, len(ids)),
		}
		for _, id := range ids {
			group.ComponentIds = append(group.ComponentIds, UUIDTo(id))
		}
		result.Groups = append(result.Groups, group)
	}

	return result
}
