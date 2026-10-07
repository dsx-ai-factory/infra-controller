// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package target

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestRegistry_Lookup(t *testing.T) {
	componentID := uuid.New()
	rackID := uuid.New()
	tests := []struct {
		name          string
		request       ResolveRequest
		want          []Target
		wantErr       string
		wantLookupErr string
	}{
		{
			name: "component",
			request: targetRequest(
				eventrule.TargetStrategyComponent,
				eventrule.ResolvedResource{
					Kind:          eventrule.ResourceKindComponent,
					ID:            componentID,
					RackID:        rackID,
					ComponentType: flowtypes.ComponentTypeCompute,
				},
			),
			want: []Target{{
				Kind:   eventrule.ResourceKindComponent,
				ID:     componentID,
				RackID: rackID,
			}},
		},
		{
			name: "component rejects rack resource",
			request: targetRequest(
				eventrule.TargetStrategyComponent,
				eventrule.ResolvedResource{
					Kind:   eventrule.ResourceKindRack,
					ID:     rackID,
					RackID: rackID,
				},
			),
			wantErr: "requires a component resource",
		},
		{
			name: "rack from component",
			request: targetRequest(
				eventrule.TargetStrategyRack,
				eventrule.ResolvedResource{
					Kind:          eventrule.ResourceKindComponent,
					ID:            componentID,
					RackID:        rackID,
					ComponentType: flowtypes.ComponentTypeCompute,
				},
			),
			want: []Target{{Kind: eventrule.ResourceKindRack, ID: rackID, RackID: rackID}},
		},
		{
			name: "affected components from component",
			request: targetRequest(
				eventrule.TargetStrategyAffectedComponents,
				eventrule.ResolvedResource{
					Kind:          eventrule.ResourceKindComponent,
					ID:            componentID,
					RackID:        rackID,
					ComponentType: flowtypes.ComponentTypeCompute,
				},
			),
			want: []Target{{Kind: eventrule.ResourceKindComponent, ID: componentID, RackID: rackID}},
		},
		{
			name: "affected components from rack",
			request: targetRequest(
				eventrule.TargetStrategyAffectedComponents,
				eventrule.ResolvedResource{
					Kind:   eventrule.ResourceKindRack,
					ID:     rackID,
					RackID: rackID,
				},
			),
			want: []Target{{Kind: eventrule.ResourceKindRack, ID: rackID, RackID: rackID}},
		},
		{
			name: "invalid strategy",
			request: targetRequest(
				eventrule.TargetStrategy("unknown"),
				eventrule.ResolvedResource{
					Kind:          eventrule.ResourceKindComponent,
					ID:            componentID,
					RackID:        rackID,
					ComponentType: flowtypes.ComponentTypeCompute,
				},
			),
			wantLookupErr: "unknown target strategy",
		},
		{
			name: "strategy without resolver",
			request: targetRequest(
				eventrule.TargetStrategyNone,
				eventrule.ResolvedResource{
					Kind:          eventrule.ResourceKindComponent,
					ID:            componentID,
					RackID:        rackID,
					ComponentType: flowtypes.ComponentTypeCompute,
				},
			),
			wantLookupErr: "no target resolver",
		},
	}

	registry := New()
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			resolver, err := registry.Lookup(
				test.request.EventType,
				test.request.Strategy,
			)
			if test.wantLookupErr != "" {
				require.ErrorContains(t, err, test.wantLookupErr)
				require.Nil(t, resolver)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, resolver)
			resolved, err := resolver.Resolve(context.Background(), test.request)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.Nil(t, resolved)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, resolved)
		})
	}

	t.Run("event-specific resolver overrides generic resolver", func(t *testing.T) {
		specificRackID := uuid.New()
		want := []Target{{
			Kind: eventrule.ResourceKindRack, ID: specificRackID, RackID: specificRackID,
		}}
		registry := New()
		require.NoError(t, registry.Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyComponent,
			&staticResolver{targets: want},
		))

		request := targetRequest(
			eventrule.TargetStrategyComponent,
			eventrule.ResolvedResource{
				Kind:          eventrule.ResourceKindComponent,
				ID:            componentID,
				RackID:        rackID,
				ComponentType: flowtypes.ComponentTypeCompute,
			},
		)
		resolver, err := registry.Lookup(request.EventType, request.Strategy)
		require.NoError(t, err)
		require.NotNil(t, resolver)
		resolved, err := resolver.Resolve(context.Background(), request)
		require.NoError(t, err)
		require.Equal(t, want, resolved)
	})
}

func TestRegistry_Register(t *testing.T) {
	resolver := &staticResolver{}

	t.Run("event-specific strategy", func(t *testing.T) {
		require.NoError(t, New().Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyAffectedComponents,
			resolver,
		))
	})
	t.Run("duplicate", func(t *testing.T) {
		registry := New()
		require.NoError(t, registry.Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyAffectedComponents,
			resolver,
		))
		require.Error(t, registry.Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyAffectedComponents,
			resolver,
		))
	})
	t.Run("override generic strategy", func(t *testing.T) {
		require.NoError(t, New().Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyComponent,
			resolver,
		))
	})
	t.Run("struct resolver", func(t *testing.T) {
		registered := &staticResolver{}
		registry := New()
		require.NoError(t, registry.Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyAffectedComponents,
			registered,
		))

		resolved, err := registry.Lookup(
			"hardware.leak.detected",
			eventrule.TargetStrategyAffectedComponents,
		)
		require.NoError(t, err)
		require.Same(t, registered, resolved)
	})
	t.Run("nil resolver", func(t *testing.T) {
		require.Error(t, New().Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyAffectedComponents,
			nil,
		))
	})
	t.Run("strategy without resolution", func(t *testing.T) {
		require.Error(t, New().Register(
			"hardware.leak.detected",
			eventrule.TargetStrategyNone,
			resolver,
		))
	})
}

func TestRegistry_ValidateRule(t *testing.T) {
	t.Run("registered event-specific resolver", func(t *testing.T) {
		rule := targetRule(eventrule.TargetStrategyAffectedComponents)
		registry := New()
		require.NoError(t, registry.Register(
			rule.EventType,
			eventrule.TargetStrategyAffectedComponents,
			&staticResolver{},
		))
		require.NoError(t, registry.ValidateRule(rule))
	})
	t.Run("generic resolver", func(t *testing.T) {
		for _, strategy := range []eventrule.TargetStrategy{
			eventrule.TargetStrategyComponent,
			eventrule.TargetStrategyRack,
			eventrule.TargetStrategyAffectedComponents,
		} {
			require.NoError(t, New().ValidateRule(targetRule(strategy)))
		}
	})
	t.Run("actions without targets", func(t *testing.T) {
		tests := []struct {
			name string
			spec eventrule.ActionSpec
		}{
			{name: "send alert", spec: &eventrule.SendAlert{Severity: eventrule.SeverityWarning}},
			{name: "noop", spec: &eventrule.Noop{}},
		}
		for _, test := range tests {
			t.Run(test.name, func(t *testing.T) {
				rule := targetRule(eventrule.TargetStrategyComponent)
				rule.Actions[0] = eventrule.Action{
					Name: "action",
					Spec: test.spec,
				}
				require.NoError(t, New().ValidateRule(rule))
			})
		}
	})
}

type staticResolver struct {
	targets []Target
}

func (r *staticResolver) Resolve(
	context.Context,
	ResolveRequest,
) ([]Target, error) {
	return r.targets, nil
}

func targetRequest(
	strategy eventrule.TargetStrategy,
	resource eventrule.ResolvedResource,
) ResolveRequest {
	return ResolveRequest{
		EventType: "hardware.leak.detected",
		Resource:  resource,
		Strategy:  strategy,
	}
}

func targetRule(strategy eventrule.TargetStrategy) *eventrule.Rule {
	return &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    eventrule.RuleOriginPersisted,
		Name:      "target rule",
		Enabled:   true,
		EventType: "hardware.leak.detected",
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{
				Name: "task",
				Spec: &eventrule.SubmitTask{
					Operation: &operations.PowerControlTaskInfo{
						Operation: operations.PowerOperationForcePowerOff,
					},
					TargetStrategy:   strategy,
					ConflictStrategy: eventrule.ConflictStrategyQueue,
				},
			},
		}},
	}
}
