// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package planner

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

var _ TargetLookup = (*testTargetLookup)(nil)

func TestSelectedTargetsPercentageSelectionIsDeterministicAndUsesCeil(t *testing.T) {
	run := mustOperationRunWithSelector(t, &operationrun.PercentageSelector{
		Percentage: 25,
		Seed:       "selector-seed",
	})
	candidates := executionTargetFixtures(10)

	first, err := selectedTargets(run, candidates)
	require.NoError(t, err)
	second, err := selectedTargets(run, candidates)
	require.NoError(t, err)

	require.Len(t, first, 3)
	require.Equal(t, executionTargetRackIDs(first), executionTargetRackIDs(second))
}

func TestOrderExecutionTargetsRandomOrderingUsesOrderingSeed(t *testing.T) {
	targets := executionTargetFixtures(8)

	first, err := orderExecutionTargets(targets, randomOrdering("ordering-seed-a"))
	require.NoError(t, err)
	second, err := orderExecutionTargets(targets, randomOrdering("ordering-seed-b"))
	require.NoError(t, err)

	require.NotEqual(t, executionTargetRackIDs(first), executionTargetRackIDs(second))
}

func TestOrderExecutionTargetsRejectsPhysicalLocationOrdering(t *testing.T) {
	_, err := orderExecutionTargets(executionTargetFixtures(3), physicalLocationOrdering())

	require.ErrorContains(t, err, "physical_location ordering is not supported")
}

func TestPhaseTargetCountsAssignsEqualPhases(t *testing.T) {
	counts, err := phaseTargetCounts(
		10,
		operationrun.PhasePolicy{Plan: &operationrun.EqualPhases{PhaseCount: 3}},
	)
	require.NoError(t, err)

	require.Equal(t, []int{4, 3, 3}, counts)
}

func TestPhaseTargetCountsAssignsPercentagePhases(t *testing.T) {
	counts, err := phaseTargetCounts(
		10,
		operationrun.PhasePolicy{
			Plan: &operationrun.PercentagePhases{
				Phases: []operationrun.PercentagePhase{
					{Percentage: 10},
					{Percentage: 30},
					{Percentage: 60},
				},
			},
		},
	)
	require.NoError(t, err)

	require.Equal(t, []int{1, 3, 6}, counts)
}

func TestPhaseTargetCountsAssignsCountPhasesWithFinalRemainder(t *testing.T) {
	counts, err := phaseTargetCounts(
		10,
		operationrun.PhasePolicy{
			Plan: &operationrun.CountPhases{
				Phases: []operationrun.CountPhase{{Count: 2}, {Count: 3}},
			},
		},
	)
	require.NoError(t, err)

	require.Equal(t, []int{2, 3, 5}, counts)
}

func TestPhaseTargetCountsSkipsEmptyCountPhaseRemainder(t *testing.T) {
	counts, err := phaseTargetCounts(
		5,
		operationrun.PhasePolicy{
			Plan: &operationrun.CountPhases{
				Phases: []operationrun.CountPhase{{Count: 2}, {Count: 3}},
			},
		},
	)
	require.NoError(t, err)

	require.Equal(t, []int{2, 3}, counts)
}

func TestPhaseTargetCountsRejectsCountPhasesThatExceedSelection(t *testing.T) {
	_, err := phaseTargetCounts(
		5,
		operationrun.PhasePolicy{
			Plan: &operationrun.CountPhases{
				Phases: []operationrun.CountPhase{{Count: 3}, {Count: 3}},
			},
		},
	)

	require.ErrorContains(t, err, "count phase counts exceed selected target count")
}

func TestPhasedExecutionTargetsRejectsEmptyPhases(t *testing.T) {
	tests := []struct {
		name        string
		targetCount int
		policy      operationrun.PhasePolicy
		wantErr     string
	}{
		{
			name:        "percentage phase rounds to zero for two targets",
			targetCount: 2,
			policy: operationrun.PhasePolicy{
				Plan: &operationrun.PercentagePhases{
					Phases: []operationrun.PercentagePhase{
						{Percentage: 10},
						{Percentage: 30},
						{Percentage: 60},
					},
				},
			},
			wantErr: "percentage phase 2 resolves to zero targets for 2 selected targets",
		},
		{
			name:        "percentage phase rounds to zero for five targets",
			targetCount: 5,
			policy: operationrun.PhasePolicy{
				Plan: &operationrun.PercentagePhases{
					Phases: []operationrun.PercentagePhase{
						{Percentage: 10},
						{Percentage: 10},
						{Percentage: 80},
					},
				},
			},
			wantErr: "percentage phase 2 resolves to zero targets for 5 selected targets",
		},
		{
			name:        "equal phases outnumber targets",
			targetCount: 2,
			policy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 10},
			},
			wantErr: "equal phase 2 resolves to zero targets for 2 selected targets",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			targets, err := phasedExecutionTargets(
				executionTargetFixtures(tt.targetCount),
				tt.policy,
			)

			require.Nil(t, targets)
			require.ErrorIs(t, err, ErrPhaseHasNoTargets)
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestPlanAssignsSequenceAndPhaseIndexes(t *testing.T) {
	lookup := &testTargetLookup{
		defaultScope: executionTargetFixtures(10),
	}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	targets, err := planner.Plan(context.Background(), mustOperationRun(t,
		&operationrun.PercentageSelector{
			Percentage: 100,
			Seed:       "selector-seed",
		},
		operationrun.Options{
			MaxConcurrentTargets: 1,
			ConflictPolicy:       retryConflictPolicy(),
			OrderingPolicy:       randomOrdering("ordering-seed"),
			PhasePolicy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 3},
			},
		},
		operationrun.Operation{
			Payload: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
			},
		},
	))
	require.NoError(t, err)

	require.Equal(t, []int32{0, 1, 2, 3, 4, 5, 6, 7, 8, 9}, targetSequenceIndexes(targets))
	require.Equal(t, map[int32]int{0: 4, 1: 3, 2: 3}, phaseCounts(targets))
	require.Equal(t, operationrun.OperationRunTargetStatusPending, targets[0].Status)
}

func TestPlanSkipsSelectorAndOptionsDecodeWhenCandidateScopeIsEmpty(t *testing.T) {
	lookup := &testTargetLookup{}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	operationRaw, err := operationrun.MarshalConfig(operationrun.Operation{
		Payload: &operations.FirmwareControlTaskInfo{
			Operation: operations.FirmwareOperationUpgrade,
		},
	})
	require.NoError(t, err)

	targets, err := planner.Plan(context.Background(), &operationrun.OperationRun{
		Selector:          []byte("{"),
		Options:           []byte("{"),
		OperationTemplate: operationRaw,
	})
	require.NoError(t, err)
	require.Empty(t, targets)
	require.Equal(t, 1, lookup.defaultScopeCalls)
}

func TestExcludeExecutionTargets(t *testing.T) {
	excluded, err := executionTargets(
		executionTargetsFromIDs(2, 3),
	).componentsByRackID()
	require.NoError(t, err)

	scope := executionTargets(executionTargetsFromIDs(1, 2, 3, 4)).exclude(excluded)

	require.Equal(t, executionTargets(executionTargetsFromIDs(1, 4)), scope)
}

func TestExcludeExecutionTargetsNarrowsResolvedComponents(t *testing.T) {
	base := executionTargets{
		{
			RackID: numberedUUID(1),
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute: {
					componentUUID(1, 1),
					componentUUID(1, 2),
				},
			},
		},
	}
	exclude := map[uuid.UUID]operation.ComponentsByType{
		numberedUUID(1): {
			devicetypes.ComponentTypeCompute: {componentUUID(1, 2)},
		},
	}

	scope := base.exclude(exclude)

	require.Len(t, scope, 1)
	require.Equal(
		t,
		[]uuid.UUID{componentUUID(1, 1)},
		scope[0].ComponentsByType[devicetypes.ComponentTypeCompute],
	)
}

func TestExcludeExecutionTargetsRemovesEmptyTarget(t *testing.T) {
	excluded, err := executionTargets(
		executionTargetsFromIDs(1),
	).componentsByRackID()
	require.NoError(t, err)

	scope := executionTargets(executionTargetsFromIDs(1)).exclude(excluded)

	require.Empty(t, scope)
}

func TestExecutionTargetsComponentsByRackID(t *testing.T) {
	got, err := executionTargets{
		{
			RackID: numberedUUID(1),
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute: {componentUUID(1, 2)},
			},
		},
	}.componentsByRackID()
	require.NoError(t, err)

	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeCompute: {componentUUID(1, 2)},
		},
		got[numberedUUID(1)],
	)
}

func TestExecutionTargetsComponentsByRackIDMergesRepeatedRackRows(t *testing.T) {
	got, err := executionTargets{
		{
			RackID: numberedUUID(1),
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute: {componentUUID(1, 1)},
			},
		},
		{
			RackID: numberedUUID(1),
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute:  {componentUUID(1, 2)},
				devicetypes.ComponentTypeNVSwitch: {componentUUID(1, 3)},
			},
		},
	}.componentsByRackID()
	require.NoError(t, err)

	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeCompute: {
				componentUUID(1, 1),
				componentUUID(1, 2),
			},
			devicetypes.ComponentTypeNVSwitch: {componentUUID(1, 3)},
		},
		got[numberedUUID(1)],
	)
}

func TestPlanExcludesPriorRunTargetsFromTargetSpecScope(t *testing.T) {
	lookup := &testTargetLookup{
		targetSpec: executionTargetsFromIDs(1, 2),
		priorRuns:  executionTargetsFromIDs(2, 3),
	}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	priorRunID := uuid.New()
	targets, err := planner.Plan(context.Background(), mustOperationRun(t,
		&operationrun.PercentageSelector{
			Percentage: 100,
			Seed:       "selector-seed",
		},
		operationrun.Options{
			MaxConcurrentTargets: 1,
			ConflictPolicy: operationrun.ConflictPolicy{
				Payload: &operationrun.ConflictRetryPolicy{
					RetryTimeout:      time.Hour,
					InitialRetryDelay: time.Second,
					MaxRetryDelay:     time.Minute,
				},
			},
			OrderingPolicy: randomOrdering("ordering-seed"),
			PhasePolicy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 1},
			},
		},
		operationrun.Operation{
			TargetSpec: &operation.TargetSpec{
				Components: []operation.ComponentTarget{{UUID: numberedUUID(99)}},
			},
			TargetScope: operationrun.OperationTargetScope{
				ExcludedOperationRunIDs: []uuid.UUID{priorRunID},
			},
			Payload: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
			},
		},
	))
	require.NoError(t, err)

	require.Len(t, targets, 1)
	require.Equal(t, numberedUUID(1), targets[0].RackID)
	require.Equal(t, operationrun.OperationRunTargetStatusPending, targets[0].Status)
	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeCompute: {componentUUID(1, 1)},
		},
		targets[0].ComponentsByType,
	)
	require.Equal(t, TargetLookupOptions{MaxTargets: 10}, lookup.targetSpecOptions)
	require.Equal(t, TargetLookupOptions{MaxTargets: 10}, lookup.priorRunsOptions)
}

func TestPlanMergesRepeatedPriorRunTargetsBeforeExclusion(t *testing.T) {
	rackID := numberedUUID(1)
	lookup := &testTargetLookup{
		targetSpec: []operation.RackExecutionTarget{
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {
						componentUUID(1, 1),
						componentUUID(1, 2),
						componentUUID(1, 3),
					},
				},
			},
		},
		priorRuns: []operation.RackExecutionTarget{
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {componentUUID(1, 1)},
				},
			},
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {componentUUID(1, 2)},
				},
			},
		},
	}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	targets, err := planner.Plan(context.Background(), mustOperationRun(t,
		&operationrun.PercentageSelector{
			Percentage: 100,
			Seed:       "selector-seed",
		},
		operationrun.Options{
			MaxConcurrentTargets: 1,
			ConflictPolicy:       retryConflictPolicy(),
			OrderingPolicy:       randomOrdering("ordering-seed"),
			PhasePolicy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 1},
			},
		},
		operationrun.Operation{
			TargetSpec: &operation.TargetSpec{
				Components: []operation.ComponentTarget{{UUID: numberedUUID(99)}},
			},
			TargetScope: operationrun.OperationTargetScope{
				ExcludedOperationRunIDs: []uuid.UUID{uuid.New()},
			},
			Payload: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
			},
		},
	))
	require.NoError(t, err)

	require.Len(t, targets, 1)
	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeCompute: {componentUUID(1, 3)},
		},
		targets[0].ComponentsByType,
	)
}

func TestPlanUsesExplicitEmptyTargetSpecWithoutDefaultFallback(t *testing.T) {
	lookup := &testTargetLookup{
		defaultScope: executionTargetsFromIDs(1, 2),
	}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	targets, err := planner.Plan(context.Background(), mustOperationRun(t,
		&operationrun.PercentageSelector{
			Percentage: 100,
			Seed:       "selector-seed",
		},
		operationrun.Options{
			MaxConcurrentTargets: 1,
			ConflictPolicy:       retryConflictPolicy(),
			OrderingPolicy:       randomOrdering("ordering-seed"),
			PhasePolicy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 1},
			},
		},
		operationrun.Operation{
			TargetSpec: &operation.TargetSpec{
				Components: []operation.ComponentTarget{{UUID: numberedUUID(99)}},
			},
			Payload: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
			},
		},
	))
	require.NoError(t, err)

	require.Empty(t, targets)
	require.Zero(t, lookup.defaultScopeCalls)
	require.Equal(t, 1, lookup.targetSpecCalls)
}

func TestPlanExcludesPriorRunTargetsFromDefaultScope(t *testing.T) {
	lookup := &testTargetLookup{
		defaultScope: executionTargetsFromIDs(1, 2, 3),
		priorRuns:    executionTargetsFromIDs(2),
	}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	priorRunID := uuid.New()
	targets, err := planner.Plan(context.Background(), mustOperationRun(t,
		&operationrun.PercentageSelector{
			Percentage: 100,
			Seed:       "selector-seed",
		},
		operationrun.Options{
			MaxConcurrentTargets: 1,
			ConflictPolicy:       retryConflictPolicy(),
			OrderingPolicy:       randomOrdering("ordering-seed"),
			PhasePolicy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 1},
			},
		},
		operationrun.Operation{
			TargetScope: operationrun.OperationTargetScope{
				ExcludedOperationRunIDs: []uuid.UUID{priorRunID},
			},
			Payload: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
			},
		},
	))
	require.NoError(t, err)

	require.ElementsMatch(
		t,
		[]uuid.UUID{numberedUUID(1), numberedUUID(3)},
		targetRackIDs(targets),
	)
	require.Equal(t, 1, lookup.defaultScopeCalls)
	require.Equal(t, 1, lookup.priorRunsCalls)
	require.Equal(t, TargetLookupOptions{MaxTargets: 10}, lookup.defaultScopeOptions)
	require.Equal(t, TargetLookupOptions{MaxTargets: 10}, lookup.priorRunsOptions)
}

func TestPlanPassesDefaultScopeComponentFilterToLookup(t *testing.T) {
	lookup := &testTargetLookup{
		defaultScope: executionTargetsFromIDs(1),
	}
	planner := New(lookup, Config{MaxCandidateScopeTargets: 10})

	_, err := planner.Plan(context.Background(), mustOperationRun(t,
		&operationrun.PercentageSelector{
			Percentage: 100,
			Seed:       "selector-seed",
		},
		operationrun.Options{
			MaxConcurrentTargets: 1,
			ConflictPolicy:       retryConflictPolicy(),
			OrderingPolicy:       randomOrdering("ordering-seed"),
			PhasePolicy: operationrun.PhasePolicy{
				Plan: &operationrun.EqualPhases{PhaseCount: 1},
			},
		},
		operationrun.Operation{
			TargetScope: operationrun.OperationTargetScope{
				DefaultScopeComponentFilter: &operation.ComponentFilter{
					Kind:  operation.ComponentFilterKindTypes,
					Types: []string{"Compute"},
				},
			},
			Payload: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
			},
		},
	))
	require.NoError(t, err)

	require.NotNil(t, lookup.defaultScopeOperation)
	filter := lookup.defaultScopeOperation.TargetScope.DefaultScopeComponentFilter
	require.NotNil(t, filter)
	require.Equal(t, operation.ComponentFilterKindTypes, filter.Kind)
	require.Equal(t, []string{"Compute"}, filter.Types)
}

type testTargetLookup struct {
	defaultScope []operation.RackExecutionTarget
	targetSpec   []operation.RackExecutionTarget
	priorRuns    []operation.RackExecutionTarget

	defaultScopeOperation *operationrun.Operation
	defaultScopeOptions   TargetLookupOptions
	targetSpecOptions     TargetLookupOptions
	priorRunsOptions      TargetLookupOptions

	defaultScopeCalls int
	targetSpecCalls   int
	priorRunsCalls    int
}

func (l *testTargetLookup) TargetsFromDefaultScope(
	_ context.Context,
	op *operationrun.Operation,
	opts TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	l.defaultScopeCalls++
	l.defaultScopeOperation = op
	l.defaultScopeOptions = opts
	return l.defaultScope, nil
}

func (l *testTargetLookup) TargetsFromSpec(
	_ context.Context,
	_ *operation.TargetSpec,
	opts TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	l.targetSpecCalls++
	l.targetSpecOptions = opts
	return l.targetSpec, nil
}

func (l *testTargetLookup) TargetsFromRuns(
	_ context.Context,
	_ []uuid.UUID,
	opts TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	l.priorRunsCalls++
	l.priorRunsOptions = opts
	return l.priorRuns, nil
}

func mustOperationRun(
	t *testing.T,
	selector operationrun.Selector,
	options operationrun.Options,
	operation operationrun.Operation,
) *operationrun.OperationRun {
	t.Helper()

	selectorRaw, err := operationrun.MarshalConfig(selector)
	require.NoError(t, err)
	optionsRaw, err := operationrun.MarshalConfig(options)
	require.NoError(t, err)
	operationRaw, err := operationrun.MarshalConfig(operation)
	require.NoError(t, err)

	return &operationrun.OperationRun{
		Selector:          selectorRaw,
		Options:           optionsRaw,
		OperationTemplate: operationRaw,
	}
}

func mustOperationRunWithSelector(
	t *testing.T,
	selector operationrun.Selector,
) *operationrun.OperationRun {
	t.Helper()

	selectorRaw, err := operationrun.MarshalConfig(selector)
	require.NoError(t, err)

	return &operationrun.OperationRun{Selector: selectorRaw}
}

func randomOrdering(seed string) operationrun.OrderingPolicy {
	return operationrun.OrderingPolicy{Payload: &operationrun.RandomOrdering{Seed: seed}}
}

func physicalLocationOrdering() operationrun.OrderingPolicy {
	return operationrun.OrderingPolicy{
		Payload: &operationrun.PhysicalLocationOrdering{
			Strategy: operationrun.PhysicalLocationOrderingStrategyRowByRow,
		},
	}
}

func retryConflictPolicy() operationrun.ConflictPolicy {
	return operationrun.ConflictPolicy{
		Payload: &operationrun.ConflictRetryPolicy{
			RetryTimeout:      time.Hour,
			InitialRetryDelay: time.Second,
			MaxRetryDelay:     time.Minute,
		},
	}
}

func executionTargetFixtures(count int) []operation.RackExecutionTarget {
	result := make([]operation.RackExecutionTarget, 0, count)
	for i := range count {
		id := i + 1
		result = append(result, executionTargetFromID(id))
	}
	return result
}

func executionTargetsFromIDs(ids ...int) []operation.RackExecutionTarget {
	result := make([]operation.RackExecutionTarget, 0, len(ids))
	for _, id := range ids {
		result = append(result, executionTargetFromID(id))
	}
	return result
}

func executionTargetFromID(id int) operation.RackExecutionTarget {
	return operation.RackExecutionTarget{
		RackID: numberedUUID(id),
		ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
			devicetypes.ComponentTypeCompute: {componentUUID(id, 1)},
		},
	}
}

func componentUUID(rack, component int) uuid.UUID {
	return mustUUID(fmt.Sprintf(
		"10000000-0000-0000-0000-%06d%06d",
		rack,
		component,
	))
}

func numberedUUID(n int) uuid.UUID {
	return mustUUID(fmt.Sprintf("00000000-0000-0000-0000-%012d", n))
}

func mustUUID(value string) uuid.UUID {
	id, err := uuid.Parse(value)
	if err != nil {
		panic(err)
	}
	return id
}

func executionTargetRackIDs(targets []operation.RackExecutionTarget) []uuid.UUID {
	ids := make([]uuid.UUID, 0, len(targets))
	for _, target := range targets {
		ids = append(ids, target.RackID)
	}
	return ids
}

func targetRackIDs(targets []*operationrun.OperationRunTarget) []uuid.UUID {
	ids := make([]uuid.UUID, 0, len(targets))
	for _, target := range targets {
		ids = append(ids, target.RackID)
	}
	return ids
}

func targetSequenceIndexes(targets []*operationrun.OperationRunTarget) []int32 {
	indexes := make([]int32, 0, len(targets))
	for _, target := range targets {
		indexes = append(indexes, target.SequenceIndex)
	}
	return indexes
}

func phaseCounts(targets []*operationrun.OperationRunTarget) map[int32]int {
	counts := make(map[int32]int)
	for _, target := range targets {
		counts[target.PhaseIndex]++
	}
	return counts
}
