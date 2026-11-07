// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"slices"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestActionsValidate(t *testing.T) {
	conditions := []ActionCondition{
		{},
		{Severities: []Severity{SeverityWarning, SeverityCritical}},
		{ComponentTypes: []flowtypes.ComponentType{flowtypes.ComponentTypeCompute}},
		{
			Severities:     []Severity{SeverityCritical},
			ComponentTypes: []flowtypes.ComponentType{flowtypes.ComponentTypeNVSwitch},
		},
	}
	strategies := []TargetStrategy{
		TargetStrategyComponent,
		TargetStrategyRack,
		TargetStrategyAffectedComponents,
	}

	actions := make([]Action, 0, len(strategies)+2)
	for i, strategy := range strategies {
		actions = append(actions, Action{
			Name:      []string{"component", "rack", "affected"}[i],
			Condition: conditions[i],
			Spec: &SubmitTask{
				Operation: &operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationForcePowerOff,
				},
				TargetStrategy:   strategy,
				ConflictStrategy: ConflictStrategyQueue,
			},
		})
	}
	actions = append(actions,
		Action{
			Name:      "alert",
			Condition: conditions[3],
			Spec: &SendAlert{
				Severity: SeverityCritical,
				Message:  "Leak detected",
			},
		},
		Action{Name: "noop", Spec: &Noop{Reason: "record only"}},
	)
	wantStrategies := append(
		slices.Clone(strategies),
		TargetStrategyNone,
		TargetStrategyNone,
	)

	for i := range actions {
		require.NoError(t, actions[i].Validate())
		require.Equal(
			t,
			wantStrategies[i],
			actions[i].Spec.TargetResolutionStrategy(),
		)
	}
}

func TestValidateActions(t *testing.T) {
	valid := Action{Name: "noop", Spec: &Noop{}}
	tests := map[string]struct {
		actions []Action
		wantErr string
	}{
		"nil":   {},
		"empty": {actions: []Action{}},
		"valid": {actions: []Action{valid}},
		"invalid action": {
			actions: []Action{{Name: "invalid"}},
			wantErr: "actions[0]: action spec is required",
		},
		"duplicate name": {
			actions: []Action{valid, valid},
			wantErr: `actions[1]: duplicate action name "noop"`,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := ValidateActions(test.actions)
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.EqualError(t, err, test.wantErr)
		})
	}
}

func TestActionRejectsInvalidDomainValues(t *testing.T) {
	tests := map[string]Action{
		"empty condition list": {
			Name:      "noop",
			Condition: ActionCondition{Severities: []Severity{}},
			Spec:      &Noop{},
		},
		"unspecified severity": {
			Name: "noop",
			Condition: ActionCondition{
				Severities: []Severity{SeverityUnspecified},
			},
			Spec: &Noop{},
		},
		"unspecified alert severity": {
			Name: "alert",
			Spec: &SendAlert{Severity: SeverityUnspecified},
		},
		"unknown strategy": {
			Name: "task",
			Spec: &SubmitTask{
				Operation: &operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationForcePowerOff,
				},
				TargetStrategy:   "unknown",
				ConflictStrategy: ConflictStrategyQueue,
			},
		},
		"task without target resolution": {
			Name: "task",
			Spec: &SubmitTask{
				Operation: &operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationForcePowerOff,
				},
				TargetStrategy:   TargetStrategyNone,
				ConflictStrategy: ConflictStrategyQueue,
			},
		},
		"invalid operation": {
			Name: "task",
			Spec: &SubmitTask{
				Operation:        &operations.PowerControlTaskInfo{},
				TargetStrategy:   TargetStrategyComponent,
				ConflictStrategy: ConflictStrategyQueue,
			},
		},
		"missing operation": {
			Name: "task",
			Spec: &SubmitTask{
				TargetStrategy:   TargetStrategyComponent,
				ConflictStrategy: ConflictStrategyQueue,
			},
		},
		"typed nil operation": {
			Name: "task",
			Spec: &SubmitTask{
				Operation:        (*operations.PowerControlTaskInfo)(nil),
				TargetStrategy:   TargetStrategyComponent,
				ConflictStrategy: ConflictStrategyQueue,
			},
		},
		"missing spec": {Name: "task"},
	}

	for name, action := range tests {
		t.Run(name, func(t *testing.T) {
			require.Error(t, action.Validate())
		})
	}

	typedNilSpecs := map[string]ActionSpec{
		"submit task": (*SubmitTask)(nil),
		"send alert":  (*SendAlert)(nil),
		"noop":        (*Noop)(nil),
	}
	for name, spec := range typedNilSpecs {
		t.Run("nil "+name+" spec", func(t *testing.T) {
			err := (Action{Name: "action", Spec: spec}).Validate()
			require.EqualError(t, err, "action spec is required")
		})
	}
}

func TestAction_Clone(t *testing.T) {
	tests := map[string]ActionSpec{
		"submit task": &SubmitTask{
			Operation: &operations.FirmwareControlTaskInfo{
				Operation:  operations.FirmwareOperationUpgrade,
				SubTargets: []string{"bmc"},
			},
			Description: "submit",
		},
		"send alert": &SendAlert{Message: "alert"},
		"noop":       &Noop{Reason: "noop"},
	}

	for name, spec := range tests {
		t.Run(name, func(t *testing.T) {
			action := Action{
				Name: "action",
				Condition: ActionCondition{
					Severities: []Severity{SeverityWarning},
				},
				Spec: spec,
			}

			cloned := action.Clone()
			require.Equal(t, action, cloned)
			require.NotSame(t, action.Spec, cloned.Spec)

			cloned.Condition.Severities[0] = SeverityCritical
			require.Equal(t, SeverityWarning, action.Condition.Severities[0])
		})
	}

	t.Run("submit task operation", func(t *testing.T) {
		action := Action{
			Spec: &SubmitTask{
				Operation: &operations.FirmwareControlTaskInfo{
					Operation:  operations.FirmwareOperationUpgrade,
					SubTargets: []string{"bmc"},
				},
			},
		}

		cloned := action.Clone()
		originalSpec := action.Spec.(*SubmitTask)
		clonedSpec := cloned.Spec.(*SubmitTask)
		require.NotSame(t, originalSpec.Operation, clonedSpec.Operation)
		clonedOperation := clonedSpec.Operation.(*operations.FirmwareControlTaskInfo)
		clonedOperation.SubTargets[0] = "bios"
		require.Equal(
			t,
			[]string{"bmc"},
			originalSpec.Operation.(*operations.FirmwareControlTaskInfo).SubTargets,
		)
	})

	t.Run("nil specs", func(t *testing.T) {
		require.Nil(t, (Action{}).Clone().Spec)

		cloned := (Action{Spec: (*Noop)(nil)}).Clone()
		require.True(t, cloned.Spec == nil)
	})
}

func TestActionType_Validate(t *testing.T) {
	tests := map[string]struct {
		actionType ActionType
		wantErr    bool
	}{
		"submit task": {actionType: ActionTypeSubmitTask},
		"send alert":  {actionType: ActionTypeSendAlert},
		"noop":        {actionType: ActionTypeNoop},
		"unspecified": {wantErr: true},
		"unknown":     {actionType: "unknown", wantErr: true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.actionType.Validate()
			if test.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestTargetStrategy_RequiresResolution(t *testing.T) {
	tests := map[string]struct {
		strategy TargetStrategy
		want     bool
	}{
		"none":                {strategy: TargetStrategyNone},
		"component":           {strategy: TargetStrategyComponent, want: true},
		"rack":                {strategy: TargetStrategyRack, want: true},
		"affected components": {strategy: TargetStrategyAffectedComponents, want: true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.NoError(t, test.strategy.Validate())
			require.Equal(t, test.want, test.strategy.RequiresResolution())
		})
	}
}

func TestSubmitTask_TargetResolutionStrategy(t *testing.T) {
	tests := map[string]struct {
		spec *SubmitTask
		want TargetStrategy
	}{
		"nil": {want: TargetStrategyNone},
		"strategy": {
			spec: &SubmitTask{TargetStrategy: TargetStrategyRack},
			want: TargetStrategyRack,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, test.spec.TargetResolutionStrategy())
		})
	}
}

func TestRuleValidatesPolicy(t *testing.T) {
	action := Action{Name: "noop", Spec: &Noop{}}
	rule := Rule{
		ID:        uuid.New(),
		Origin:    RuleOriginPersisted,
		Name:      "test rule",
		Enabled:   true,
		EventType: "test.event",
		Policy: Policy{
			Actions: []Action{action},
		},
	}
	require.NoError(t, rule.Validate())

	rule.Actions = nil
	require.ErrorContains(t, rule.Validate(), "actions are required")
	rule.Actions = []Action{action, action}
	require.ErrorContains(t, rule.Validate(), "duplicate action name")
	rule.Actions = []Action{action}
	rule.Origin = ""
	require.ErrorContains(t, rule.Validate(), "unknown rule origin")
}

func TestActionConditionAppliesTo(t *testing.T) {
	condition := ActionCondition{
		Severities:     []Severity{SeverityWarning, SeverityCritical},
		ComponentTypes: []flowtypes.ComponentType{flowtypes.ComponentTypeCompute},
	}

	tests := map[string]struct {
		condition ActionCondition
		envelope  Envelope
		resource  ResolvedResource
		want      bool
	}{
		"matches severity and component type": {
			condition: condition,
			envelope:  Envelope{Severity: SeverityCritical},
			resource: ResolvedResource{
				Kind:          ResourceKindComponent,
				ComponentType: flowtypes.ComponentTypeCompute,
			},
			want: true,
		},
		"rejects severity": {
			condition: condition,
			envelope:  Envelope{Severity: SeverityInfo},
			resource: ResolvedResource{
				Kind:          ResourceKindComponent,
				ComponentType: flowtypes.ComponentTypeCompute,
			},
		},
		"rejects component type": {
			condition: condition,
			envelope:  Envelope{Severity: SeverityCritical},
			resource: ResolvedResource{
				Kind:          ResourceKindComponent,
				ComponentType: flowtypes.ComponentTypeNVSwitch,
			},
		},
		"component type condition rejects rack": {
			condition: condition,
			envelope:  Envelope{Severity: SeverityCritical},
			resource: ResolvedResource{
				Kind:          ResourceKindRack,
				ComponentType: flowtypes.ComponentTypeCompute,
			},
		},
		"empty severity set matches nothing": {
			condition: ActionCondition{Severities: []Severity{}},
			envelope:  Envelope{Severity: SeverityCritical},
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, test.want, test.condition.AppliesTo(
				test.envelope,
				test.resource,
			))
		})
	}
}
