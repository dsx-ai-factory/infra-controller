// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package planner builds deterministic operation-run rows from resolved rack
// execution targets and run configuration.
package planner

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// ErrPhaseHasNoTargets reports that a user-configured phase cannot be
// materialized for the selected target set.
var ErrPhaseHasNoTargets = errors.New("configured operation run phase has no targets")

// TargetLookup supplies the primitive target lists needed by the planner.
// The planner decides which sources to use and how to compose them; lookup
// implementations own DB-backed inventory and run target reads.
type TargetLookup interface {
	TargetsFromDefaultScope(
		ctx context.Context,
		operation *operationrun.Operation,
		opts TargetLookupOptions,
	) ([]operation.RackExecutionTarget, error)

	TargetsFromSpec(
		ctx context.Context,
		spec *operation.TargetSpec,
		opts TargetLookupOptions,
	) ([]operation.RackExecutionTarget, error)

	TargetsFromRuns(
		ctx context.Context,
		runIDs []uuid.UUID,
		opts TargetLookupOptions,
	) ([]operation.RackExecutionTarget, error)
}

// TargetLookupOptions carries planner limits into lookup implementations so
// they can enforce them while querying instead of returning an oversized slice.
type TargetLookupOptions struct {
	// MaxTargets limits how many rack execution targets one lookup may return.
	// Zero means no explicit limit.
	MaxTargets int
}

// Planner builds the frozen execution plan for an operation run.
type Planner interface {
	Plan(
		ctx context.Context,
		run *operationrun.OperationRun,
	) ([]*operationrun.OperationRunTarget, error)
}

// Config controls planner behavior that is not part of an individual run.
type Config struct {
	// MaxCandidateScopeTargets protects memory during target lookup. Lookup
	// implementations should query at most MaxCandidateScopeTargets+1 rows and
	// return an error when the limit is exceeded. Zero means no explicit limit.
	MaxCandidateScopeTargets int
}

const (
	// DefaultMaxCandidateScopeTargets is the service-level safety limit for
	// candidate-scope lookup until operation-run config grows an explicit knob.
	DefaultMaxCandidateScopeTargets = 100
)

// PlannerImpl implements Planner.
type PlannerImpl struct {
	lookup TargetLookup
	config Config
}

// New creates a planner that resolves target sources with lookup.
func New(lookup TargetLookup, config Config) *PlannerImpl {
	return &PlannerImpl{lookup: lookup, config: config}
}

var _ Planner = (*PlannerImpl)(nil)

// Plan resolves candidate sources, composes the candidate scope, and
// returns the materialized operation-run targets for all planned phases.
func (p *PlannerImpl) Plan(
	ctx context.Context,
	run *operationrun.OperationRun,
) ([]*operationrun.OperationRunTarget, error) {
	if p == nil || p.lookup == nil {
		return nil, fmt.Errorf("operation run target lookup is required")
	}

	if run == nil {
		return nil, fmt.Errorf("operation run is required")
	}

	candidates, err := p.candidates(ctx, run)
	if err != nil {
		return nil, err
	}

	selected, err := selectedTargets(run, candidates)
	if err != nil {
		return nil, err
	}

	targets, err := plannedTargets(run, selected)
	if err != nil {
		return nil, err
	}

	return targets, nil
}

// candidates resolves the decoded operation's inclusive scope and subtracts
// any previously planned run targets that the operation excludes.
func (p *PlannerImpl) candidates(
	ctx context.Context,
	run *operationrun.OperationRun,
) ([]operation.RackExecutionTarget, error) {
	op, err := run.DecodedOperation()
	if err != nil {
		return nil, err
	}

	lookupOptions := TargetLookupOptions{
		MaxTargets: p.config.MaxCandidateScopeTargets,
	}

	base, err := p.baseCandidateScope(ctx, op, lookupOptions)
	if err != nil {
		return nil, err
	}

	if len(base) == 0 {
		// Quick exit when the base scope is empty.
		return base, nil
	}

	excluded, err := p.excludedCandidateScope(ctx, op, lookupOptions)
	if err != nil {
		return nil, err
	}

	return executionTargets(base).exclude(excluded), nil
}

// selectedTargets applies the run selector to the candidate scope. Empty
// candidate scopes deliberately skip selector decoding so an empty operation
// can plan to zero targets without requiring selector/options materialization.
func selectedTargets(
	run *operationrun.OperationRun,
	candidates []operation.RackExecutionTarget,
) ([]operation.RackExecutionTarget, error) {
	if len(candidates) == 0 {
		return nil, nil
	}

	selector, err := run.DecodedSelector()
	if err != nil {
		return nil, err
	}

	switch s := selector.(type) {
	case *operationrun.PercentageSelector:
		return selectPercentageTargets(candidates, s), nil
	default:
		return nil, fmt.Errorf("unsupported selector kind %q", selector.SelectorKind())
	}
}

// plannedTargets applies ordering, then freezes the ordered targets into
// phase-aware operation-run target rows.
func plannedTargets(
	run *operationrun.OperationRun,
	selected []operation.RackExecutionTarget,
) ([]*operationrun.OperationRunTarget, error) {
	if len(selected) == 0 {
		return nil, nil
	}

	options, err := run.DecodedOptions()
	if err != nil {
		return nil, err
	}

	ordered, err := orderExecutionTargets(selected, options.OrderingPolicy)
	if err != nil {
		return nil, err
	}

	targets, err := phasedExecutionTargets(ordered, options.PhasePolicy)
	if err != nil {
		return nil, err
	}

	return targets, nil
}

// phasedExecutionTargets partitions ordered execution targets into phases and
// materializes the durable rows that freeze sequence and phase assignment.
func phasedExecutionTargets(
	ordered []operation.RackExecutionTarget,
	policy operationrun.PhasePolicy,
) ([]*operationrun.OperationRunTarget, error) {
	phaseCounts, err := phaseTargetCounts(len(ordered), policy)
	if err != nil {
		return nil, err
	}

	targets := make([]*operationrun.OperationRunTarget, 0, len(ordered))
	seqIdx := 0
	for phaseIdx, phaseSize := range phaseCounts {
		if phaseSize == 0 {
			return nil, fmt.Errorf(
				"%w: %s phase %d resolves to zero targets for %d selected targets",
				ErrPhaseHasNoTargets,
				policy.Plan.PhasePlanKind(),
				phaseIdx+1,
				len(ordered),
			)
		}

		for range phaseSize {
			targets = append(
				targets,
				&operationrun.OperationRunTarget{
					RackID:           ordered[seqIdx].RackID,
					SequenceIndex:    int32(seqIdx),
					PhaseIndex:       int32(phaseIdx),
					ComponentsByType: ordered[seqIdx].ComponentsByType.Clone(),
					Status:           operationrun.OperationRunTargetStatusPending,
				},
			)
			seqIdx++
		}
	}

	return targets, nil
}

// baseCandidateScope resolves the operation's inclusive source: an explicit
// target spec when present, otherwise the operation-type default scope.
func (p *PlannerImpl) baseCandidateScope(
	ctx context.Context,
	op *operationrun.Operation,
	lookupOptions TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	buildTargetScope := func() ([]operation.RackExecutionTarget, string, error) {
		var targets []operation.RackExecutionTarget
		var err error
		var source string

		if op.TargetSpec != nil {
			// The scope is explicitly defined by the target spec.
			source = "target_spec scope"
			targets, err = p.lookup.TargetsFromSpec(ctx, op.TargetSpec, lookupOptions)
		} else {
			// No explicit inclusive source was configured, so the default scope is the
			// base. This is intentionally delayed to avoid fetching all racks when an
			// explicit inclusive source already defines the base scope.
			source = "default scope"
			targets, err = p.lookup.TargetsFromDefaultScope(ctx, op, lookupOptions)
		}

		return targets, source, err
	}

	targets, source, err := buildTargetScope()
	if err != nil {
		return nil, fmt.Errorf("build %s: %w", source, err)
	}

	normalized, err := executionTargets(targets).normalize()
	if err != nil {
		return nil, fmt.Errorf("normalize %s: %w", source, err)
	}

	return normalized, nil
}

// excludedCandidateScope resolves prior-run targets that should be subtracted
// from the base scope. The map form lets exclusion operate by rack ID.
func (p *PlannerImpl) excludedCandidateScope(
	ctx context.Context,
	op *operationrun.Operation,
	lookupOptions TargetLookupOptions,
) (map[uuid.UUID]operation.ComponentsByType, error) {
	runIDs := op.TargetScope.ExcludedOperationRunIDs
	if len(runIDs) == 0 {
		// Quick exit when there are no excluded operation runs.
		return nil, nil
	}

	excluded, err := p.lookup.TargetsFromRuns(ctx, runIDs, lookupOptions)
	if err != nil {
		return nil, fmt.Errorf("resolve excluded operation runs %s: %w", runIDs, err)
	}

	componentsByRackID, err := executionTargets(excluded).componentsByRackID()
	if err != nil {
		return nil, err
	}

	return componentsByRackID, nil
}

// selectPercentageTargets deterministically orders candidates with the selector
// seed, then takes the configured ceiling percentage. The selector has already
// been validated by OperationRun.DecodedSelector.
func selectPercentageTargets(
	candidates []operation.RackExecutionTarget,
	selector *operationrun.PercentageSelector,
) []operation.RackExecutionTarget {
	ordered := executionTargets(candidates).sortBySeedScore(selector.Seed)
	selectedCount := ceilDiv(len(ordered)*int(selector.Percentage), 100)
	return ordered[:selectedCount]
}

// orderExecutionTargets dispatches an already-validated ordering policy to the
// concrete ordering implementation used by the planner.
func orderExecutionTargets(
	targets []operation.RackExecutionTarget,
	policy operationrun.OrderingPolicy,
) ([]operation.RackExecutionTarget, error) {
	switch p := policy.Payload.(type) {
	case *operationrun.RandomOrdering:
		return orderRandomTargets(targets, p), nil
	case *operationrun.PhysicalLocationOrdering:
		return orderPhysicalLocationTargets(targets, p)
	default:
		return nil, fmt.Errorf("unsupported ordering policy kind %q", p.OrderingPolicyKind())
	}
}

// orderRandomTargets returns a deterministic seed-based order without mutating
// the input target slice.
func orderRandomTargets(
	targets []operation.RackExecutionTarget,
	policy *operationrun.RandomOrdering,
) []operation.RackExecutionTarget {
	return executionTargets(targets).sortBySeedScore(policy.Seed)
}

// orderPhysicalLocationTargets is reserved for the API shape but intentionally
// rejected until rack-location-aware ordering is implemented.
func orderPhysicalLocationTargets(
	_ []operation.RackExecutionTarget,
	_ *operationrun.PhysicalLocationOrdering,
) ([]operation.RackExecutionTarget, error) {
	return nil, fmt.Errorf("physical_location ordering is not supported yet")
}

// phaseTargetCounts converts a validated phase policy into per-phase target
// counts. Count-based phases still need the selected total to reject
// over-allocation.
func phaseTargetCounts(
	total int,
	policy operationrun.PhasePolicy,
) ([]int, error) {
	if total == 0 {
		return nil, nil
	}

	if policy.Plan == nil {
		// No phase plan means one phase with all targets.
		return []int{total}, nil
	}

	switch plan := policy.Plan.(type) {
	case *operationrun.EqualPhases:
		return equalPhaseTargetCounts(total, plan), nil
	case *operationrun.PercentagePhases:
		return percentagePhaseTargetCounts(total, plan), nil
	case *operationrun.CountPhases:
		return countPhaseTargetCounts(total, plan)
	default:
		return nil, fmt.Errorf("unsupported phase plan kind %q", policy.Plan.PhasePlanKind())
	}
}

// equalPhaseTargetCounts uses ceil boundaries so uneven totals are distributed
// across phases without losing or duplicating targets.
func equalPhaseTargetCounts(total int, plan *operationrun.EqualPhases) []int {
	counts := make([]int, int(plan.PhaseCount))
	for i := range int(plan.PhaseCount) {
		// Use cumulative boundaries so remainders spread across phases:
		// total=10, phases=3 -> [4,3,3]; total=5, phases=3 -> [2,2,1].
		start := ceilDiv(total*i, int(plan.PhaseCount))
		end := ceilDiv(total*(i+1), int(plan.PhaseCount))
		counts[i] = end - start
	}
	return counts
}

// percentagePhaseTargetCounts uses cumulative percentage endpoints so rounding
// happens against the full target count instead of compounding per phase.
func percentagePhaseTargetCounts(
	total int,
	plan *operationrun.PercentagePhases,
) []int {
	counts := make([]int, 0, len(plan.Phases))
	sum := int32(0)
	lastEnd := 0
	for _, phase := range plan.Phases {
		// Use cumulative boundaries so percentage rounding is stable:
		// total=10, percentages=[10,30,60] -> [1,3,6];
		// total=7, percentages=[50,50] -> [4,3].
		sum += phase.Percentage
		end := ceilDiv(total*int(sum), 100)
		counts = append(counts, end-lastEnd)
		lastEnd = end
	}
	return counts
}

// countPhaseTargetCounts materializes configured early phase counts and appends
// a final remainder phase covering any targets not explicitly assigned.
func countPhaseTargetCounts(total int, plan *operationrun.CountPhases) ([]int, error) {
	counts := make([]int, 0, len(plan.Phases)+1)
	assigned := 0
	for _, phase := range plan.Phases {
		// Configured counts define early phases; the final phase is the remainder:
		// total=10, counts=[2,3] -> [2,3,5]; total=5, counts=[2,3] -> [2,3].
		assigned += int(phase.Count)
		if assigned > total {
			return nil, fmt.Errorf("count phase counts exceed selected target count")
		}

		counts = append(counts, int(phase.Count))
	}

	if assigned < total {
		counts = append(counts, total-assigned)
	}
	return counts, nil
}

// ceilDiv performs integer ceiling division. A non-positive denominator is
// treated as zero output for defensive callers that validate elsewhere.
func ceilDiv(n, d int) int {
	if d <= 0 {
		return 0
	}
	return (n + d - 1) / d
}
