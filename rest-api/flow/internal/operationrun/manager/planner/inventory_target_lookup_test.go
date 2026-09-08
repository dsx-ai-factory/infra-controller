// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package planner

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	inventorycomponent "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

var _ InventoryTargetSource = (*fakeInventoryTargetSource)(nil)
var _ OperationRunTargetSource = (*fakeOperationRunTargetSource)(nil)

func TestInventoryTargetLookupTargetsFromRackSpecFiltersComponentTypes(t *testing.T) {
	rackID := uuid.New()
	computeID := uuid.New()
	nvSwitchID := uuid.New()
	inventory := &fakeInventoryTargetSource{
		racksByID: map[uuid.UUID]*rack.Rack{
			rackID: rackWithComponents(
				rackID,
				componentWithRack(
					computeID,
					rackID,
					devicetypes.ComponentTypeCompute,
					"",
				),
				componentWithRack(
					nvSwitchID,
					rackID,
					devicetypes.ComponentTypeNVSwitch,
					"",
				),
			),
		},
	}
	lookup := NewInventoryTargetLookup(inventory, nil)

	targets, err := lookup.TargetsFromSpec(
		context.Background(),
		&operation.TargetSpec{
			Racks: []operation.RackTarget{
				{
					Identifier: identifier.Identifier{ID: rackID},
					ComponentTypes: []devicetypes.ComponentType{
						devicetypes.ComponentTypeCompute,
					},
				},
			},
		},
		TargetLookupOptions{},
	)
	require.NoError(t, err)

	require.Len(t, targets, 1)
	require.Equal(t, rackID, targets[0].RackID)
	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeCompute: {computeID},
		},
		targets[0].ComponentsByType,
	)
}

func TestInventoryTargetLookupTargetsFromComponentSpec(t *testing.T) {
	rackID := uuid.New()
	computeID := uuid.New()
	nvSwitchID := uuid.New()
	externalID := "nv-1"
	inventory := &fakeInventoryTargetSource{
		componentsByID: map[uuid.UUID]*inventorycomponent.Component{
			computeID: componentPointerWithRack(
				computeID,
				rackID,
				devicetypes.ComponentTypeCompute,
				"",
			),
		},
		externalComponents: []*inventorycomponent.Component{
			componentPointerWithRack(
				nvSwitchID,
				rackID,
				devicetypes.ComponentTypeNVSwitch,
				externalID,
			),
		},
	}
	lookup := NewInventoryTargetLookup(inventory, nil)

	targets, err := lookup.TargetsFromSpec(
		context.Background(),
		&operation.TargetSpec{
			Components: []operation.ComponentTarget{
				{UUID: computeID},
				{
					External: &operation.ExternalRef{
						Type: devicetypes.ComponentTypeUnknown,
						ID:   externalID,
					},
				},
			},
		},
		TargetLookupOptions{},
	)
	require.NoError(t, err)

	require.Equal(
		t,
		[]operation.RackExecutionTarget{
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {computeID},
				},
			},
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeNVSwitch: {nvSwitchID},
				},
			},
		},
		targets,
	)
}

func TestInventoryTargetLookupTargetsFromNVLDomainSpec(t *testing.T) {
	t.Run("filters component types across domain racks", func(t *testing.T) {
		domainID := uuid.New()
		rackOneID := uuid.New()
		rackTwoID := uuid.New()
		computeOneID := uuid.New()
		computeTwoID := uuid.New()
		inventory := &fakeInventoryTargetSource{
			domainRacks: map[uuid.UUID][]*rack.Rack{
				domainID: {
					rackWithComponents(rackOneID),
					rackWithComponents(rackTwoID),
				},
			},
			racksByID: map[uuid.UUID]*rack.Rack{
				rackOneID: rackWithComponents(
					rackOneID,
					componentWithRack(
						computeOneID,
						rackOneID,
						devicetypes.ComponentTypeCompute,
						"",
					),
				),
				rackTwoID: rackWithComponents(
					rackTwoID,
					componentWithRack(
						computeTwoID,
						rackTwoID,
						devicetypes.ComponentTypeCompute,
						"",
					),
				),
			},
		}

		targets, err := NewInventoryTargetLookup(inventory, nil).TargetsFromSpec(
			context.Background(),
			&operation.TargetSpec{
				NVLDomains: []operation.NVLDomainTarget{
					{
						Identifier: identifier.Identifier{ID: domainID},
						ComponentTypes: []devicetypes.ComponentType{
							devicetypes.ComponentTypeCompute,
						},
					},
				},
			},
			TargetLookupOptions{},
		)
		require.NoError(t, err)
		require.Equal(t, []operation.RackExecutionTarget{
			{
				RackID: rackOneID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {computeOneID},
				},
			},
			{
				RackID: rackTwoID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {computeTwoID},
				},
			},
		}, targets)
	})

	t.Run("merges repeated racks before enforcing limit", func(t *testing.T) {
		domainID := uuid.New()
		rackID := uuid.New()
		computeID := uuid.New()
		nvSwitchID := uuid.New()
		inventory := &fakeInventoryTargetSource{
			domainRacks: map[uuid.UUID][]*rack.Rack{
				domainID: {rackWithComponents(rackID)},
			},
			racksByID: map[uuid.UUID]*rack.Rack{
				rackID: rackWithComponents(
					rackID,
					componentWithRack(computeID, rackID, devicetypes.ComponentTypeCompute, ""),
					componentWithRack(nvSwitchID, rackID, devicetypes.ComponentTypeNVSwitch, ""),
				),
			},
		}

		targets, err := NewInventoryTargetLookup(inventory, nil).TargetsFromSpec(
			context.Background(),
			&operation.TargetSpec{
				NVLDomains: []operation.NVLDomainTarget{
					{
						Identifier:     identifier.Identifier{ID: domainID},
						ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeCompute},
					},
					{
						Identifier:     identifier.Identifier{ID: domainID},
						ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeNVSwitch},
					},
				},
			},
			TargetLookupOptions{MaxTargets: 1},
		)
		require.NoError(t, err)
		require.Equal(t, []operation.RackExecutionTarget{
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute:  {computeID},
					devicetypes.ComponentTypeNVSwitch: {nvSwitchID},
				},
			},
		}, targets)
	})
}

func TestInventoryTargetLookupTargetsFromDefaultScopeAppliesComponentFilter(t *testing.T) {
	rackID := uuid.New()
	computeID := uuid.New()
	nvSwitchID := uuid.New()
	inventory := &fakeInventoryTargetSource{
		racks: []*rack.Rack{
			rackWithComponents(
				rackID,
				componentWithRack(
					computeID,
					rackID,
					devicetypes.ComponentTypeCompute,
					"",
				),
				componentWithRack(
					nvSwitchID,
					rackID,
					devicetypes.ComponentTypeNVSwitch,
					"",
				),
			),
		},
	}
	lookup := NewInventoryTargetLookup(inventory, nil)

	targets, err := lookup.TargetsFromDefaultScope(
		context.Background(),
		&operationrun.Operation{
			TargetScope: operationrun.OperationTargetScope{
				DefaultScopeComponentFilter: &operation.ComponentFilter{
					Kind: operation.ComponentFilterKindComponents,
					Components: []uuid.UUID{
						nvSwitchID,
					},
				},
			},
		},
		TargetLookupOptions{},
	)
	require.NoError(t, err)

	require.Len(t, targets, 1)
	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeNVSwitch: {nvSwitchID},
		},
		targets[0].ComponentsByType,
	)
}

func TestInventoryTargetLookupTargetsFromDefaultScopeRejectsLimitOverflow(t *testing.T) {
	rackID1 := uuid.New()
	rackID2 := uuid.New()
	inventory := &fakeInventoryTargetSource{
		racks: []*rack.Rack{
			rackWithComponents(
				rackID1,
				componentWithRack(
					uuid.New(),
					rackID1,
					devicetypes.ComponentTypeCompute,
					"",
				),
			),
			rackWithComponents(
				rackID2,
				componentWithRack(
					uuid.New(),
					rackID2,
					devicetypes.ComponentTypeCompute,
					"",
				),
			),
		},
	}
	lookup := NewInventoryTargetLookup(inventory, nil)

	_, err := lookup.TargetsFromDefaultScope(
		context.Background(),
		&operationrun.Operation{},
		TargetLookupOptions{MaxTargets: 1},
	)

	require.ErrorContains(t, err, "candidate scope exceeds target limit 1")
	require.Len(t, inventory.paginations, 1)
	require.Equal(t, 2, inventory.paginations[0].Limit)
}

func TestInventoryTargetLookupTargetsFromDefaultScopeLimitsFilteredTargets(t *testing.T) {
	computeRackID1 := uuid.New()
	computeRackID2 := uuid.New()
	nvSwitchRackID := uuid.New()
	nvSwitchID := uuid.New()
	inventory := &fakeInventoryTargetSource{
		racks: []*rack.Rack{
			rackWithComponents(
				computeRackID1,
				componentWithRack(
					uuid.New(),
					computeRackID1,
					devicetypes.ComponentTypeCompute,
					"",
				),
			),
			rackWithComponents(
				computeRackID2,
				componentWithRack(
					uuid.New(),
					computeRackID2,
					devicetypes.ComponentTypeCompute,
					"",
				),
			),
			rackWithComponents(
				nvSwitchRackID,
				componentWithRack(
					nvSwitchID,
					nvSwitchRackID,
					devicetypes.ComponentTypeNVSwitch,
					"",
				),
			),
		},
	}
	lookup := NewInventoryTargetLookup(inventory, nil)

	targets, err := lookup.TargetsFromDefaultScope(
		context.Background(),
		&operationrun.Operation{
			TargetScope: operationrun.OperationTargetScope{
				DefaultScopeComponentFilter: &operation.ComponentFilter{
					Kind:  operation.ComponentFilterKindTypes,
					Types: []string{"NVSwitch"},
				},
			},
		},
		TargetLookupOptions{MaxTargets: 1},
	)
	require.NoError(t, err)

	require.Len(t, targets, 1)
	require.Equal(t, nvSwitchRackID, targets[0].RackID)
	require.Equal(
		t,
		operation.ComponentsByType{
			devicetypes.ComponentTypeNVSwitch: {nvSwitchID},
		},
		targets[0].ComponentsByType,
	)
	require.Equal(
		t,
		[]dbquery.Pagination{
			{Offset: 0, Limit: 2},
			{Offset: 2, Limit: 2},
		},
		inventory.paginations,
	)
}

func TestInventoryTargetLookupTargetsFromRunsReadsAllMaterializedTargets(t *testing.T) {
	runID := uuid.New()
	rackID := uuid.New()
	componentID := uuid.New()
	store := &fakeOperationRunTargetSource{
		targets: []*operationrun.OperationRunTarget{
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {componentID},
				},
			},
		},
	}
	lookup := NewInventoryTargetLookup(nil, store)

	targets, err := lookup.TargetsFromRuns(
		context.Background(),
		[]uuid.UUID{runID},
		TargetLookupOptions{},
	)
	require.NoError(t, err)

	require.Equal(t, []uuid.UUID{runID}, store.runIDs)
	require.Equal(
		t,
		operationrun.TargetPhaseScopeAllMaterializedTargets,
		store.opts.PhaseScope,
	)
	require.Equal(
		t,
		[]operation.RackExecutionTarget{
			{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {componentID},
				},
			},
		},
		targets,
	)
}

type fakeInventoryTargetSource struct {
	racks              []*rack.Rack
	racksByID          map[uuid.UUID]*rack.Rack
	domainRacks        map[uuid.UUID][]*rack.Rack
	componentsByID     map[uuid.UUID]*inventorycomponent.Component
	externalComponents []*inventorycomponent.Component
	total              int32
	paginations        []dbquery.Pagination
}

func (s *fakeInventoryTargetSource) GetRacksForNVLDomain(
	_ context.Context,
	id identifier.Identifier,
) ([]*rack.Rack, error) {
	return s.domainRacks[id.ID], nil
}

func (s *fakeInventoryTargetSource) GetRackByIdentifier(
	_ context.Context,
	id identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	if rack, ok := s.racksByID[id.ID]; ok {
		return rack, nil
	}

	return nil, fmt.Errorf("rack %s not found", id.ID)
}

func (s *fakeInventoryTargetSource) GetListOfRacks(
	_ context.Context,
	_ dbquery.StringQueryInfo,
	_ *dbquery.StringQueryInfo,
	_ *dbquery.StringQueryInfo,
	pagination *dbquery.Pagination,
	_ *dbquery.OrderBy,
	_ bool,
) ([]*rack.Rack, int32, error) {
	if pagination != nil {
		s.paginations = append(s.paginations, *pagination)
	}
	total := s.total
	if total == 0 {
		total = int32(len(s.racks))
	}

	racks := s.racks
	if pagination != nil {
		start := min(pagination.Offset, len(racks))
		end := min(start+pagination.Limit, len(racks))
		racks = racks[start:end]
	}

	return racks, total, nil
}

func (s *fakeInventoryTargetSource) GetComponentByID(
	_ context.Context,
	id uuid.UUID,
) (*inventorycomponent.Component, error) {
	if comp, ok := s.componentsByID[id]; ok {
		return comp, nil
	}

	return nil, fmt.Errorf("component %s not found", id)
}

func (s *fakeInventoryTargetSource) GetComponentByBMCMAC(
	_ context.Context,
	macAddress string,
) (*inventorycomponent.Component, error) {
	return nil, status.Errorf(codes.NotFound, "component with BMC MAC %s not found", macAddress)
}

func (s *fakeInventoryTargetSource) GetComponentsByExternalIDs(
	_ context.Context,
	_ []string,
) ([]*inventorycomponent.Component, error) {
	return s.externalComponents, nil
}

type fakeOperationRunTargetSource struct {
	targets []*operationrun.OperationRunTarget
	runIDs  []uuid.UUID
	opts    operationrun.TargetListOptions
}

func (s *fakeOperationRunTargetSource) ListTargets(
	_ context.Context,
	runID uuid.UUID,
	opts operationrun.TargetListOptions,
) ([]*operationrun.OperationRunTarget, int32, error) {
	s.runIDs = append(s.runIDs, runID)
	s.opts = opts
	return s.targets, int32(len(s.targets)), nil
}

func rackWithComponents(
	id uuid.UUID,
	components ...inventorycomponent.Component,
) *rack.Rack {
	return &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: id},
		Components: components,
	}
}

func componentWithRack(
	id uuid.UUID,
	rackID uuid.UUID,
	componentType devicetypes.ComponentType,
	externalID string,
) inventorycomponent.Component {
	return inventorycomponent.Component{
		Type: componentType,
		Info: deviceinfo.DeviceInfo{
			ID: id,
		},
		ComponentID: externalID,
		RackID:      rackID,
	}
}

func componentPointerWithRack(
	id uuid.UUID,
	rackID uuid.UUID,
	componentType devicetypes.ComponentType,
	externalID string,
) *inventorycomponent.Component {
	comp := componentWithRack(id, rackID, componentType, externalID)
	return &comp
}
