// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakage

import (
	"context"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

var _ InventoryResolver = (*inventoryresolver.Resolver)(nil)

func TestDefaultRuleValidates(t *testing.T) {
	rule := DefaultRule()
	require.NoError(t, rule.Validate())
	assert.Equal(t, uuid.MustParse("f34b87f7-cb1b-4b08-aa51-30c0b3b58680"), rule.ID)
	assert.Equal(t, eventrule.RuleOriginBuiltIn, rule.Origin)
	assert.Equal(t, TypeHardwareLeakDetected, rule.EventType)
	require.Len(t, rule.Actions, 1)
	spec, ok := rule.Actions[0].Spec.(*eventrule.SubmitTask)
	require.True(t, ok)
	operation, ok := spec.Operation.(*operations.PowerControlTaskInfo)
	require.True(t, ok)
	assert.Equal(t, operations.PowerOperationForcePowerOff, operation.Operation)
	assert.Equal(t, eventrule.TargetStrategyAffectedComponents, spec.TargetStrategy)
}

func TestRegisterTargetResolvers(t *testing.T) {
	registry := target.New()
	require.NoError(t, RegisterTargetResolvers(registry, &targetInventory{}))
	rule := DefaultRule()
	require.NoError(t, registry.ValidateRule(rule))
}

func TestTargetResolver_ResolveAffectedComponents(t *testing.T) {
	t.Run("source and components below it", testResolveAffectedComponents)
	t.Run("whole rack", testResolveAffectedComponentsRack)
	t.Run("source only", testResolveAffectedComponentsSourceOnly)
	t.Run("inventory error", testResolveAffectedComponentsError)
	t.Run("malformed topology", testResolveAffectedComponentsMalformedTopology)
}

func testResolveAffectedComponentsRack(t *testing.T) {
	rackID := uuid.New()
	inventory := &targetInventory{}
	registry := target.New()
	require.NoError(t, RegisterTargetResolvers(registry, inventory))

	request := target.ResolveRequest{
		EventType: TypeHardwareLeakDetected,
		Resource: eventrule.ResolvedResource{
			Kind:   eventrule.ResourceKindRack,
			ID:     rackID,
			RackID: rackID,
		},
		Strategy: eventrule.TargetStrategyAffectedComponents,
	}
	resolver, err := registry.Lookup(request.EventType, request.Strategy)
	require.NoError(t, err)
	targets, err := resolver.Resolve(context.Background(), request)
	require.NoError(t, err)
	require.Equal(t, []target.Target{{
		Kind:   eventrule.ResourceKindRack,
		ID:     rackID,
		RackID: rackID,
	}}, targets)
	require.False(t, inventory.withComponents)
}

func TestAffectedComponentIDs(t *testing.T) {
	rackID := uuid.New()
	sourceID := uuid.MustParse("00000000-0000-0000-0000-000000000003")
	belowID := uuid.MustParse("00000000-0000-0000-0000-000000000001")

	tests := []struct {
		name       string
		components []component.Component
		want       []uuid.UUID
		wantErr    string
	}{
		{
			name: "selects source and components below it",
			components: []component.Component{
				inventoryComponent(uuid.New(), rackID, 20),
				inventoryComponent(sourceID, rackID, 10),
				inventoryComponent(belowID, rackID, 1),
			},
			want: []uuid.UUID{belowID, sourceID},
		},
		{
			name: "excludes components with invalid slots",
			components: []component.Component{
				inventoryComponent(sourceID, rackID, 10),
				inventoryComponent(belowID, rackID, 1),
				inventoryComponent(uuid.New(), rackID, -1),
			},
			want: []uuid.UUID{belowID, sourceID},
		},
		{
			name: "source absent",
			components: []component.Component{
				inventoryComponent(belowID, rackID, 1),
			},
			wantErr: "has no valid position",
		},
		{
			name: "source has negative slot",
			components: []component.Component{
				inventoryComponent(sourceID, rackID, -1),
			},
			wantErr: "has no valid position",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			ids, err := affectedComponentIDs(&rack.Rack{
				Info:       deviceinfo.DeviceInfo{ID: rackID},
				Components: test.components,
			}, sourceID)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.Nil(t, ids)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, ids)
		})
	}
}

func testResolveAffectedComponents(t *testing.T) {
	rackID := uuid.New()
	sourceID := uuid.MustParse("00000000-0000-0000-0000-000000000003")
	firstBelowID := uuid.MustParse("00000000-0000-0000-0000-000000000001")
	secondBelowID := uuid.MustParse("00000000-0000-0000-0000-000000000002")
	aboveID := uuid.MustParse("00000000-0000-0000-0000-000000000004")
	inventory := &targetInventory{
		rack: &rack.Rack{
			Info: deviceinfo.DeviceInfo{ID: rackID},
			Components: []component.Component{
				inventoryComponent(aboveID, rackID, 20),
				inventoryComponent(sourceID, rackID, 10),
				inventoryComponent(secondBelowID, rackID, 5),
				inventoryComponent(firstBelowID, rackID, 1),
			},
		},
	}
	registry := target.New()
	require.NoError(t, RegisterTargetResolvers(registry, inventory))

	request := target.ResolveRequest{
		EventType: TypeHardwareLeakDetected,
		Resource: eventrule.ResolvedResource{
			Kind:          eventrule.ResourceKindComponent,
			ID:            sourceID,
			RackID:        rackID,
			ComponentType: flowtypes.ComponentTypeCompute,
		},
		Strategy: eventrule.TargetStrategyAffectedComponents,
	}
	resolver, err := registry.Lookup(request.EventType, request.Strategy)
	require.NoError(t, err)
	targets, err := resolver.Resolve(context.Background(), request)
	require.NoError(t, err)
	require.Equal(t, []target.Target{
		{Kind: eventrule.ResourceKindComponent, ID: firstBelowID, RackID: rackID},
		{Kind: eventrule.ResourceKindComponent, ID: secondBelowID, RackID: rackID},
		{Kind: eventrule.ResourceKindComponent, ID: sourceID, RackID: rackID},
	}, targets)
	require.True(t, inventory.withComponents)
}

func testResolveAffectedComponentsSourceOnly(t *testing.T) {
	rackID := uuid.New()
	sourceID := uuid.New()
	inventory := &targetInventory{
		rack: &rack.Rack{
			Info:       deviceinfo.DeviceInfo{ID: rackID},
			Components: []component.Component{inventoryComponent(sourceID, rackID, 1)},
		},
	}
	registry := target.New()
	require.NoError(t, RegisterTargetResolvers(registry, inventory))

	request := target.ResolveRequest{
		EventType: TypeHardwareLeakDetected,
		Resource: eventrule.ResolvedResource{
			Kind:          eventrule.ResourceKindComponent,
			ID:            sourceID,
			RackID:        rackID,
			ComponentType: flowtypes.ComponentTypeCompute,
		},
		Strategy: eventrule.TargetStrategyAffectedComponents,
	}
	resolver, err := registry.Lookup(request.EventType, request.Strategy)
	require.NoError(t, err)
	targets, err := resolver.Resolve(context.Background(), request)
	require.NoError(t, err)
	require.Equal(t, []target.Target{{
		Kind:   eventrule.ResourceKindComponent,
		ID:     sourceID,
		RackID: rackID,
	}}, targets)
}

func testResolveAffectedComponentsError(t *testing.T) {
	registry := target.New()
	require.NoError(t, RegisterTargetResolvers(registry, &targetInventory{
		rackErr: errors.New("inventory unavailable"),
	}))
	request := target.ResolveRequest{
		EventType: TypeHardwareLeakDetected,
		Resource: eventrule.ResolvedResource{
			Kind:          eventrule.ResourceKindComponent,
			ID:            uuid.New(),
			RackID:        uuid.New(),
			ComponentType: flowtypes.ComponentTypeCompute,
		},
		Strategy: eventrule.TargetStrategyAffectedComponents,
	}
	resolver, err := registry.Lookup(request.EventType, request.Strategy)
	require.NoError(t, err)
	targets, err := resolver.Resolve(context.Background(), request)
	require.ErrorContains(t, err, "inventory unavailable")
	require.Nil(t, targets)
}

func testResolveAffectedComponentsMalformedTopology(t *testing.T) {
	rackID := uuid.New()
	sourceID := uuid.New()
	registry := target.New()
	require.NoError(t, RegisterTargetResolvers(registry, &targetInventory{
		rack: &rack.Rack{
			Info: deviceinfo.DeviceInfo{ID: rackID},
			Components: []component.Component{
				inventoryComponent(uuid.New(), rackID, 1),
			},
		},
	}))

	request := target.ResolveRequest{
		EventType: TypeHardwareLeakDetected,
		Resource: eventrule.ResolvedResource{
			Kind:          eventrule.ResourceKindComponent,
			ID:            sourceID,
			RackID:        rackID,
			ComponentType: flowtypes.ComponentTypeCompute,
		},
		Strategy: eventrule.TargetStrategyAffectedComponents,
	}
	resolver, err := registry.Lookup(request.EventType, request.Strategy)
	require.NoError(t, err)
	targets, err := resolver.Resolve(context.Background(), request)
	require.ErrorContains(t, err, "has no valid position")
	require.ErrorIs(t, err, target.ErrUnresolvable)
	require.Nil(t, targets)
}

type targetInventory struct {
	rack           *rack.Rack
	rackErr        error
	withComponents bool
}

func (i *targetInventory) RackByID(
	_ context.Context,
	_ uuid.UUID,
	withComponents bool,
) (*rack.Rack, error) {
	i.withComponents = withComponents
	return i.rack, i.rackErr
}

func inventoryComponent(id, rackID uuid.UUID, slot int) component.Component {
	return component.Component{
		Info:     deviceinfo.DeviceInfo{ID: id},
		RackID:   rackID,
		Position: component.InRackPosition{SlotID: slot},
	}
}
