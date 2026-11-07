// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"fmt"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// -----------------------------------------------------------------------------
// Selectors
// -----------------------------------------------------------------------------

// SelectorKind identifies the strategy used to choose operation-run targets
// from the resolved candidate scope.
type SelectorKind string

const (
	SelectorKindPercentage SelectorKind = "percentage"
)

// Selector is implemented by concrete target selector configurations. The kind
// discriminator lets selectors round-trip through JSONB while keeping
// dispatcher code type-directed.
type Selector interface {
	SelectorKind() SelectorKind
	Validate() error
}

// PercentageSelector selects a deterministic percentage of candidate targets.
type PercentageSelector struct {
	Percentage int32  `json:"percentage"`
	Seed       string `json:"seed"`
}

func (*PercentageSelector) SelectorKind() SelectorKind {
	return SelectorKindPercentage
}

func (s *PercentageSelector) Validate() error {
	if s == nil {
		return fmt.Errorf("percentage selector is required")
	}
	if s.Percentage < 1 || s.Percentage > 100 {
		return fmt.Errorf("percentage selector must be between 1 and 100")
	}
	if s.Seed == "" {
		return fmt.Errorf("percentage selector seed is required")
	}

	return nil
}

// -----------------------------------------------------------------------------
// Options
// -----------------------------------------------------------------------------

// Options contains dispatcher-facing controls that apply to every operation in
// a run.
type Options struct {
	MaxConcurrentTargets int32          `json:"max_concurrent_targets"`
	SafetyPolicy         SafetyPolicy   `json:"safety_policy"`
	ConflictPolicy       ConflictPolicy `json:"conflict_policy"`
	OrderingPolicy       OrderingPolicy `json:"ordering_policy"`
	PhasePolicy          PhasePolicy    `json:"phase_policy"`
}

func (o *Options) Validate() error {
	if o == nil {
		return fmt.Errorf("options are required")
	}
	if o.MaxConcurrentTargets <= 0 {
		return fmt.Errorf("max_concurrent_targets must be greater than 0")
	}
	if err := o.SafetyPolicy.Validate(); err != nil {
		return fmt.Errorf("safety_policy: %w", err)
	}
	if err := o.ConflictPolicy.Validate(); err != nil {
		return fmt.Errorf("conflict_policy: %w", err)
	}
	if err := o.OrderingPolicy.Validate(); err != nil {
		return fmt.Errorf("ordering_policy: %w", err)
	}
	if err := o.PhasePolicy.Validate(); err != nil {
		return fmt.Errorf("phase_policy: %w", err)
	}

	return nil
}

// -----------------------------------------------------------------------------
// Safety Gates
// -----------------------------------------------------------------------------

// SafetyPolicy contains gates that stop or pause a run when any gate is
// tripped.
type SafetyPolicy struct {
	Gates []SafetyGate `json:"gates"`
}

func (p SafetyPolicy) Validate() error {
	for idx, gate := range p.Gates {
		if gate == nil {
			return fmt.Errorf("safety gate %d is required", idx)
		}
		if err := gate.Validate(); err != nil {
			return fmt.Errorf("safety gate %d: %w", idx, err)
		}
	}

	return nil
}

// SafetyGateKind identifies the metric a safety gate evaluates.
type SafetyGateKind string

const (
	SafetyGateKindFailureRate  SafetyGateKind = "failure_rate"
	SafetyGateKindFailureCount SafetyGateKind = "failure_count"
)

// SafetyGate is implemented by concrete safety-gate configurations.
// Each payload decides which phase/run stats it needs through its fields.
type SafetyGate interface {
	SafetyGateKind() SafetyGateKind
	SafetyGateScope() SafetyGateScope
	IsTripped(failed int, total int) bool
	Validate() error
}

// SafetyGateScope decides whether a gate evaluates only the active phase or
// all phases processed so far.
type SafetyGateScope string

const (
	SafetyGateScopeCurrentPhase  SafetyGateScope = "current_phase"
	SafetyGateScopeCumulativeRun SafetyGateScope = "cumulative_run"
)

// FailureRateGate trips when failures exceed the configured percentage of
// targets in its scope.
type FailureRateGate struct {
	Scope                   SafetyGateScope `json:"scope"`
	FailureThresholdPercent int32           `json:"failure_threshold_percent"`
}

func (*FailureRateGate) SafetyGateKind() SafetyGateKind {
	return SafetyGateKindFailureRate
}

func (g *FailureRateGate) SafetyGateScope() SafetyGateScope {
	return g.Scope
}

func (g *FailureRateGate) IsTripped(failed int, total int) bool {
	return total > 0 &&
		failed*100 >= int(g.FailureThresholdPercent)*total
}

func (g *FailureRateGate) Validate() error {
	if g == nil {
		return fmt.Errorf("failure rate safety gate is required")
	}
	if err := validateSafetyGateScope(g.Scope); err != nil {
		return err
	}
	if g.FailureThresholdPercent < 1 || g.FailureThresholdPercent > 100 {
		return fmt.Errorf("failure_threshold_percent must be between 1 and 100")
	}

	return nil
}

// FailureCountGate trips when failures reach the configured count in its
// scope.
type FailureCountGate struct {
	Scope                 SafetyGateScope `json:"scope"`
	FailureThresholdCount int32           `json:"failure_threshold_count"`
}

func (*FailureCountGate) SafetyGateKind() SafetyGateKind {
	return SafetyGateKindFailureCount
}

func (g *FailureCountGate) SafetyGateScope() SafetyGateScope {
	return g.Scope
}

func (g *FailureCountGate) IsTripped(failed int, total int) bool {
	return failed >= int(g.FailureThresholdCount)
}

func (g *FailureCountGate) Validate() error {
	if g == nil {
		return fmt.Errorf("failure count safety gate is required")
	}
	if err := validateSafetyGateScope(g.Scope); err != nil {
		return err
	}
	if g.FailureThresholdCount <= 0 {
		return fmt.Errorf("failure_threshold_count must be greater than 0")
	}

	return nil
}

func validateSafetyGateScope(scope SafetyGateScope) error {
	switch scope {
	case "", SafetyGateScopeCurrentPhase, SafetyGateScopeCumulativeRun:
		return nil
	default:
		return fmt.Errorf("unsupported safety gate scope %q", scope)
	}
}

// -----------------------------------------------------------------------------
// Conflict Policy
// -----------------------------------------------------------------------------

// ConflictPolicyKind identifies how the dispatcher handles target conflicts.
type ConflictPolicyKind string

const (
	ConflictPolicyKindRetry ConflictPolicyKind = "retry"
)

// ConflictPolicy is the common policy boundary for target-conflict handling.
// Payload holds the concrete strategy today; the wrapper leaves room for
// policy-wide metadata later without changing Options.
type ConflictPolicy struct {
	Payload ConflictPolicyPayload `json:"payload"`
}

func (p ConflictPolicy) Validate() error {
	if p.Payload == nil {
		return fmt.Errorf("conflict policy is required")
	}
	return p.Payload.Validate()
}

// ConflictPolicyPayload is implemented by concrete conflict-handling policies.
// Policies own retry/backoff configuration rather than scattering those fields
// on OperationRun.
type ConflictPolicyPayload interface {
	ConflictPolicyKind() ConflictPolicyKind
	Validate() error
}

// ConflictRetryPolicy retries blocked targets until RetryTimeout elapses.
type ConflictRetryPolicy struct {
	RetryTimeout      time.Duration `json:"retry_timeout"`
	InitialRetryDelay time.Duration `json:"initial_retry_delay"`
	MaxRetryDelay     time.Duration `json:"max_retry_delay"`
}

func (*ConflictRetryPolicy) ConflictPolicyKind() ConflictPolicyKind {
	return ConflictPolicyKindRetry
}

func (p *ConflictRetryPolicy) Validate() error {
	if p == nil {
		return fmt.Errorf("conflict retry policy is required")
	}
	if p.RetryTimeout <= 0 {
		return fmt.Errorf("retry_timeout must be greater than 0")
	}
	if p.InitialRetryDelay <= 0 {
		return fmt.Errorf("initial_retry_delay must be greater than 0")
	}
	if p.MaxRetryDelay <= 0 {
		return fmt.Errorf("max_retry_delay must be greater than 0")
	}
	if p.MaxRetryDelay < p.InitialRetryDelay {
		return fmt.Errorf("max_retry_delay must be greater than or equal to initial_retry_delay")
	}

	return nil
}

// -----------------------------------------------------------------------------
// Ordering Policy
// -----------------------------------------------------------------------------

// OrderingPolicyKind identifies how selected targets are ordered before phases
// are formed.
type OrderingPolicyKind string

const (
	OrderingPolicyKindRandom           OrderingPolicyKind = "random"
	OrderingPolicyKindPhysicalLocation OrderingPolicyKind = "physical_location"
)

// OrderingPolicy is the common policy boundary for selected-target ordering.
// Payload holds the concrete strategy today; the wrapper leaves room for
// policy-wide metadata later without changing Options.
type OrderingPolicy struct {
	Payload OrderingPolicyPayload `json:"payload"`
}

func (p OrderingPolicy) Validate() error {
	if p.Payload == nil {
		return fmt.Errorf("ordering policy is required")
	}
	return p.Payload.Validate()
}

// OrderingPolicyPayload is implemented by concrete target-ordering strategies.
type OrderingPolicyPayload interface {
	OrderingPolicyKind() OrderingPolicyKind
	Validate() error
}

// RandomOrdering shuffles targets using a persisted seed so retries and
// restarts can reproduce the same order.
type RandomOrdering struct {
	Seed string `json:"seed"`
}

func (*RandomOrdering) OrderingPolicyKind() OrderingPolicyKind {
	return OrderingPolicyKindRandom
}

func (p *RandomOrdering) Validate() error {
	if p == nil {
		return fmt.Errorf("random ordering policy is required")
	}
	if p.Seed == "" {
		return fmt.Errorf("random ordering seed is required")
	}

	return nil
}

// PhysicalLocationOrderingStrategy describes rack-location-aware ordering
// modes. The first dispatcher implementation may reject this policy, but the
// internal shape is reserved so the model can grow without another refactor.
type PhysicalLocationOrderingStrategy string

const (
	PhysicalLocationOrderingStrategyRowByRow            PhysicalLocationOrderingStrategy = "row_by_row"
	PhysicalLocationOrderingStrategyOnePerRowRoundRobin PhysicalLocationOrderingStrategy = "one_per_row_round_robin"
)

// PhysicalLocationOrdering orders targets using physical location metadata.
type PhysicalLocationOrdering struct {
	Strategy PhysicalLocationOrderingStrategy `json:"strategy"`
}

func (*PhysicalLocationOrdering) OrderingPolicyKind() OrderingPolicyKind {
	return OrderingPolicyKindPhysicalLocation
}

func (p *PhysicalLocationOrdering) Validate() error {
	if p == nil {
		return fmt.Errorf("physical_location ordering policy is required")
	}
	switch p.Strategy {
	case PhysicalLocationOrderingStrategyRowByRow,
		PhysicalLocationOrderingStrategyOnePerRowRoundRobin:
		return nil
	default:
		return fmt.Errorf("unsupported physical_location ordering strategy %q", p.Strategy)
	}
}

// -----------------------------------------------------------------------------
// Phase Policy
// -----------------------------------------------------------------------------

// PhasePlanKind identifies how a run is split into phases.
type PhasePlanKind string

const (
	PhasePlanKindEqual      PhasePlanKind = "equal"
	PhasePlanKindPercentage PhasePlanKind = "percentage"
	PhasePlanKindCount      PhasePlanKind = "count"
)

// PhasePlan is implemented by concrete phase-splitting configurations.
type PhasePlan interface {
	PhasePlanKind() PhasePlanKind
	Validate() error
}

// PhasePolicy combines a phase plan with the rule for advancing between
// phases.
type PhasePolicy struct {
	Plan          PhasePlan          `json:"plan"`
	AdvancePolicy PhaseAdvancePolicy `json:"advance_policy"`
}

func (p PhasePolicy) Validate() error {
	if p.Plan == nil {
		return nil
	}
	return p.Plan.Validate()
}

// EqualPhases splits selected targets into PhaseCount roughly equal phases.
type EqualPhases struct {
	PhaseCount int32 `json:"phase_count"`
}

func (*EqualPhases) PhasePlanKind() PhasePlanKind {
	return PhasePlanKindEqual
}

func (p *EqualPhases) Validate() error {
	if p == nil {
		return fmt.Errorf("equal phase policy is required")
	}
	if p.PhaseCount <= 0 {
		return fmt.Errorf("equal phase_count must be greater than 0")
	}

	return nil
}

// PercentagePhases defines explicit phase sizes as percentages of selected
// targets.
type PercentagePhases struct {
	Phases []PercentagePhase `json:"phases"`
}

func (*PercentagePhases) PhasePlanKind() PhasePlanKind {
	return PhasePlanKindPercentage
}

func (p *PercentagePhases) Validate() error {
	if p == nil {
		return fmt.Errorf("percentage phase policy is required")
	}
	if len(p.Phases) == 0 {
		return fmt.Errorf("percentage phase policy must include at least one phase")
	}

	sum := int32(0)
	for _, phase := range p.Phases {
		if phase.Percentage < 1 || phase.Percentage > 100 {
			return fmt.Errorf("percentage phase percentages must be between 1 and 100")
		}
		sum += phase.Percentage
	}
	if sum != 100 {
		return fmt.Errorf("percentage phase percentages must sum to 100")
	}

	return nil
}

// PercentagePhase describes one explicit percentage-based phase.
type PercentagePhase struct {
	Percentage int32 `json:"percentage"`
}

// CountPhases defines explicit phase sizes by target count. The final phase
// covers any remaining targets not covered by the configured counts.
type CountPhases struct {
	Phases []CountPhase `json:"phases"`
}

func (*CountPhases) PhasePlanKind() PhasePlanKind {
	return PhasePlanKindCount
}

func (p *CountPhases) Validate() error {
	if p == nil {
		return fmt.Errorf("count phase policy is required")
	}
	if len(p.Phases) == 0 {
		return fmt.Errorf("count phase policy must include at least one phase")
	}

	for _, phase := range p.Phases {
		if phase.Count <= 0 {
			return fmt.Errorf("count phase counts must be greater than 0")
		}
	}

	return nil
}

// CountPhase describes one explicit count-based phase.
type CountPhase struct {
	Count int32 `json:"count"`
}

// PhaseAdvancePolicy controls whether a successful phase advances
// automatically or waits for ResumeOperationRun.
type PhaseAdvancePolicy struct {
	AutoAdvance bool `json:"auto_advance"`
}

// -----------------------------------------------------------------------------
// Operation Template
// -----------------------------------------------------------------------------

// Operation stores the shared operation-run operation template. Payload holds
// the concrete task operation info, while the surrounding fields capture
// operation-run-only scheduling and scoping metadata.
type Operation struct {
	Type         common.TaskType       `json:"type"`
	Code         string                `json:"code,omitempty"`
	TargetSpec   *operation.TargetSpec `json:"target_spec,omitempty"`
	Description  string                `json:"description,omitempty"`
	QueueOptions *QueueOptions         `json:"queue_options,omitempty"`
	TargetScope  OperationTargetScope  `json:"target_scope"`
	Payload      operations.Operation  `json:"payload"`
}

func (o *Operation) Validate() error {
	if o == nil {
		return fmt.Errorf("operation is required")
	}
	if o.Payload == nil {
		return fmt.Errorf("operation payload is required")
	}
	if err := o.Payload.Validate(); err != nil {
		return fmt.Errorf("validate operation payload: %w", err)
	}
	if !o.Type.IsZero() && o.Type != o.Payload.Type() {
		return fmt.Errorf("operation type does not match payload")
	}
	if o.Code != "" && o.Code != o.Payload.CodeString() {
		return fmt.Errorf("operation code does not match payload")
	}
	if o.TargetSpec != nil {
		if err := o.TargetSpec.Validate(); err != nil {
			return fmt.Errorf("target_spec: %w", err)
		}
		if o.TargetScope.DefaultScopeComponentFilter != nil {
			return fmt.Errorf(
				"target_scope.default_scope_component_filter requires target_spec to be omitted",
			)
		}
	}
	if err := o.TargetScope.Validate(); err != nil {
		return fmt.Errorf("target_scope: %w", err)
	}

	return nil
}

// OperationTargetScope controls how the embedded operation target_spec and
// previous operation-run targets contribute to the candidate scope.
type OperationTargetScope struct {
	ExcludedOperationRunIDs     []uuid.UUID                `json:"excluded_operation_run_ids,omitempty"`
	DefaultScopeComponentFilter *operation.ComponentFilter `json:"default_scope_component_filter,omitempty"`
}

func (s OperationTargetScope) Validate() error {
	for idx, id := range s.ExcludedOperationRunIDs {
		if id == uuid.Nil {
			return fmt.Errorf("excluded_operation_run_ids[%d] is required", idx)
		}
	}
	if s.DefaultScopeComponentFilter != nil {
		if err := s.DefaultScopeComponentFilter.Validate(); err != nil {
			return fmt.Errorf("default_scope_component_filter: %w", err)
		}
	}

	return nil
}

// QueueOptions stores operation-level task conflict behavior using the same
// conflict strategy type as regular operation submissions.
type QueueOptions struct {
	ConflictStrategy    operation.ConflictStrategy `json:"conflict_strategy"`
	QueueTimeoutSeconds int32                      `json:"queue_timeout_seconds,omitempty"`
}
