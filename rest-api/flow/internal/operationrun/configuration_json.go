// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"encoding/json"
	"fmt"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/operation"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/common"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/operations"
)

// -----------------------------------------------------------------------------
// Interface JSON Envelopes
// -----------------------------------------------------------------------------

type optionsJSON struct {
	MaxConcurrentTargets int32          `json:"max_concurrent_targets"`
	SafetyPolicy         SafetyPolicy   `json:"safety_policy"`
	ConflictPolicy       ConflictPolicy `json:"conflict_policy"`
	OrderingPolicy       OrderingPolicy `json:"ordering_policy"`
	PhasePolicy          PhasePolicy    `json:"phase_policy"`
}

func (o Options) MarshalJSON() ([]byte, error) {
	return json.Marshal(optionsJSON{
		MaxConcurrentTargets: o.MaxConcurrentTargets,
		SafetyPolicy:         o.SafetyPolicy,
		ConflictPolicy:       o.ConflictPolicy,
		OrderingPolicy:       o.OrderingPolicy,
		PhasePolicy:          o.PhasePolicy,
	})
}

func (o *Options) UnmarshalJSON(data []byte) error {
	var raw optionsJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}

	*o = Options{
		MaxConcurrentTargets: raw.MaxConcurrentTargets,
		SafetyPolicy:         raw.SafetyPolicy,
		ConflictPolicy:       raw.ConflictPolicy,
		OrderingPolicy:       raw.OrderingPolicy,
		PhasePolicy:          raw.PhasePolicy,
	}

	return nil
}

type selectorJSON struct {
	Kind    SelectorKind    `json:"kind"`
	Payload json.RawMessage `json:"payload"`
}

func marshalSelector(selector Selector) (json.RawMessage, error) {
	if selector == nil {
		return nil, fmt.Errorf("selector is required")
	}

	payload, err := json.Marshal(selector)
	if err != nil {
		return nil, err
	}

	return json.Marshal(selectorJSON{
		Kind:    selector.SelectorKind(),
		Payload: payload,
	})
}

func unmarshalSelector(data []byte) (Selector, error) {
	var raw selectorJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}

	switch raw.Kind {
	case SelectorKindPercentage:
		selector := &PercentageSelector{}
		if err := json.Unmarshal(raw.Payload, selector); err != nil {
			return nil, err
		}
		return selector, nil
	default:
		return nil, fmt.Errorf("unsupported selector kind %q", raw.Kind)
	}
}

type safetyPolicyJSON struct {
	Gates []json.RawMessage `json:"gates"`
}

func (p SafetyPolicy) MarshalJSON() ([]byte, error) {
	gates := make([]json.RawMessage, 0, len(p.Gates))
	for _, gate := range p.Gates {
		raw, err := marshalSafetyGate(gate)
		if err != nil {
			return nil, err
		}
		gates = append(gates, raw)
	}

	return json.Marshal(safetyPolicyJSON{Gates: gates})
}

func (p *SafetyPolicy) UnmarshalJSON(data []byte) error {
	var raw safetyPolicyJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}

	gates := make([]SafetyGate, 0, len(raw.Gates))
	for _, gateRaw := range raw.Gates {
		gate, err := unmarshalSafetyGate(gateRaw)
		if err != nil {
			return err
		}
		gates = append(gates, gate)
	}

	p.Gates = gates
	return nil
}

type safetyGateJSON struct {
	Kind    SafetyGateKind  `json:"kind"`
	Payload json.RawMessage `json:"payload"`
}

func marshalSafetyGate(gate SafetyGate) (json.RawMessage, error) {
	if gate == nil {
		return nil, fmt.Errorf("safety gate is required")
	}

	payload, err := json.Marshal(gate)
	if err != nil {
		return nil, err
	}

	return json.Marshal(safetyGateJSON{
		Kind:    gate.SafetyGateKind(),
		Payload: payload,
	})
}

func unmarshalSafetyGate(data []byte) (SafetyGate, error) {
	var raw safetyGateJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}

	switch raw.Kind {
	case SafetyGateKindFailureRate:
		gate := &FailureRateGate{}
		if err := json.Unmarshal(raw.Payload, gate); err != nil {
			return nil, err
		}
		return gate, nil
	case SafetyGateKindFailureCount:
		gate := &FailureCountGate{}
		if err := json.Unmarshal(raw.Payload, gate); err != nil {
			return nil, err
		}
		return gate, nil
	default:
		return nil, fmt.Errorf("unsupported safety gate kind %q", raw.Kind)
	}
}

type conflictPolicyJSON struct {
	Kind    ConflictPolicyKind `json:"kind"`
	Payload json.RawMessage    `json:"payload"`
}

func (p ConflictPolicy) MarshalJSON() ([]byte, error) {
	if p.Payload == nil {
		return nil, fmt.Errorf("conflict policy is required")
	}

	payload, err := json.Marshal(p.Payload)
	if err != nil {
		return nil, err
	}

	return json.Marshal(conflictPolicyJSON{
		Kind:    p.Payload.ConflictPolicyKind(),
		Payload: payload,
	})
}

func (p *ConflictPolicy) UnmarshalJSON(data []byte) error {
	var raw conflictPolicyJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}

	switch raw.Kind {
	case ConflictPolicyKindRetry:
		payload := &ConflictRetryPolicy{}
		if err := json.Unmarshal(raw.Payload, payload); err != nil {
			return err
		}
		p.Payload = payload
		return nil
	default:
		return fmt.Errorf("unsupported conflict policy kind %q", raw.Kind)
	}
}

type orderingPolicyJSON struct {
	Kind    OrderingPolicyKind `json:"kind"`
	Payload json.RawMessage    `json:"payload"`
}

func (p OrderingPolicy) MarshalJSON() ([]byte, error) {
	if p.Payload == nil {
		return nil, fmt.Errorf("ordering policy is required")
	}

	payload, err := json.Marshal(p.Payload)
	if err != nil {
		return nil, err
	}

	return json.Marshal(orderingPolicyJSON{
		Kind:    p.Payload.OrderingPolicyKind(),
		Payload: payload,
	})
}

func (p *OrderingPolicy) UnmarshalJSON(data []byte) error {
	var raw orderingPolicyJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}

	switch raw.Kind {
	case OrderingPolicyKindRandom:
		payload := &RandomOrdering{}
		if err := json.Unmarshal(raw.Payload, payload); err != nil {
			return err
		}
		p.Payload = payload
		return nil
	case OrderingPolicyKindPhysicalLocation:
		payload := &PhysicalLocationOrdering{}
		if err := json.Unmarshal(raw.Payload, payload); err != nil {
			return err
		}
		p.Payload = payload
		return nil
	default:
		return fmt.Errorf("unsupported ordering policy kind %q", raw.Kind)
	}
}

type phasePolicyJSON struct {
	Kind          PhasePlanKind      `json:"kind"`
	Plan          json.RawMessage    `json:"plan"`
	AdvancePolicy PhaseAdvancePolicy `json:"advance_policy"`
}

func (p PhasePolicy) MarshalJSON() ([]byte, error) {
	if p.Plan == nil {
		return nil, fmt.Errorf("phase plan is required")
	}

	plan, err := json.Marshal(p.Plan)
	if err != nil {
		return nil, err
	}

	return json.Marshal(phasePolicyJSON{
		Kind:          p.Plan.PhasePlanKind(),
		Plan:          plan,
		AdvancePolicy: p.AdvancePolicy,
	})
}

func (p *PhasePolicy) UnmarshalJSON(data []byte) error {
	var raw phasePolicyJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}

	switch raw.Kind {
	case PhasePlanKindEqual:
		plan := &EqualPhases{}
		if err := json.Unmarshal(raw.Plan, plan); err != nil {
			return err
		}
		p.Plan = plan
	case PhasePlanKindPercentage:
		plan := &PercentagePhases{}
		if err := json.Unmarshal(raw.Plan, plan); err != nil {
			return err
		}
		p.Plan = plan
	case PhasePlanKindCount:
		plan := &CountPhases{}
		if err := json.Unmarshal(raw.Plan, plan); err != nil {
			return err
		}
		p.Plan = plan
	default:
		return fmt.Errorf("unsupported phase plan kind %q", raw.Kind)
	}

	p.AdvancePolicy = raw.AdvancePolicy
	return nil
}

// -----------------------------------------------------------------------------
// Operation Template
// -----------------------------------------------------------------------------

// operationJSON is the persisted JSONB envelope for interface-backed task
// operation payloads.
type operationJSON struct {
	Type         common.TaskType       `json:"type"`
	Code         string                `json:"code,omitempty"`
	TargetSpec   *operation.TargetSpec `json:"target_spec,omitempty"`
	Description  string                `json:"description,omitempty"`
	QueueOptions *QueueOptions         `json:"queue_options,omitempty"`
	TargetScope  OperationTargetScope  `json:"target_scope"`
	Payload      json.RawMessage       `json:"payload"`
}

func (o Operation) MarshalJSON() ([]byte, error) {
	if o.Payload == nil {
		return nil, fmt.Errorf("operation payload is required")
	}
	if err := o.Payload.Validate(); err != nil {
		return nil, fmt.Errorf("validate operation payload: %w", err)
	}

	opType := o.Type
	if opType.IsZero() {
		opType = o.Payload.Type()
	}

	opCode := o.Code
	if opCode == "" {
		opCode = o.Payload.CodeString()
	}
	if opType != o.Payload.Type() || opCode != o.Payload.CodeString() {
		return nil, fmt.Errorf("operation type/code does not match payload")
	}

	payload, err := o.Payload.Marshal()
	if err != nil {
		return nil, err
	}

	return json.Marshal(operationJSON{
		Type:         opType,
		Code:         opCode,
		TargetSpec:   o.TargetSpec,
		Description:  o.Description,
		QueueOptions: o.QueueOptions,
		TargetScope:  o.TargetScope,
		Payload:      payload,
	})
}

func (o *Operation) UnmarshalJSON(data []byte) error {
	var raw operationJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	if len(raw.Payload) == 0 {
		return fmt.Errorf("operation payload is required")
	}

	payload, err := operations.New(raw.Type, raw.Payload)
	if err != nil {
		return err
	}
	if err := payload.Validate(); err != nil {
		return fmt.Errorf("validate operation payload: %w", err)
	}

	opCode := raw.Code
	if opCode == "" {
		opCode = payload.CodeString()
	}
	if opCode != payload.CodeString() {
		return fmt.Errorf("operation code does not match payload")
	}

	*o = Operation{
		Type:         raw.Type,
		Code:         opCode,
		TargetSpec:   raw.TargetSpec,
		Description:  raw.Description,
		QueueOptions: raw.QueueOptions,
		TargetScope:  raw.TargetScope,
		Payload:      payload,
	}

	return nil
}

// -----------------------------------------------------------------------------
// Config Persistence
// -----------------------------------------------------------------------------

// MarshalConfig serializes operation-run configuration values for JSONB
// persistence.
func MarshalConfig(value any) (json.RawMessage, error) {
	if selector, ok := value.(Selector); ok {
		return marshalSelector(selector)
	}

	raw, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}

	return raw, nil
}

// UnmarshalConfig restores a JSONB-backed operation-run configuration value.
func UnmarshalConfig(raw json.RawMessage, value any) error {
	if len(raw) == 0 {
		return fmt.Errorf("stored operation-run JSON is empty")
	}

	if selector, ok := value.(*Selector); ok {
		converted, err := unmarshalSelector(raw)
		if err != nil {
			return err
		}
		*selector = converted
		return nil
	}

	return json.Unmarshal(raw, value)
}
