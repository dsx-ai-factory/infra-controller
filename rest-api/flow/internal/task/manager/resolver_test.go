// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// mockTargetFetcher is a mock implementation of TargetFetcher for testing.
type mockTargetFetcher struct {
	racks                  map[uuid.UUID]*rack.Rack
	racksByName            map[string]*rack.Rack
	domainRacks            map[uuid.UUID][]*rack.Rack
	domainRacksByName      map[string][]*rack.Rack
	components             map[uuid.UUID]*component.Component
	componentsByExternalID map[string][]*component.Component

	// Error injection
	getRackErr      error
	getDomainErr    error
	getComponentErr error
	getExternalErr  error
}

func newMockTargetFetcher() *mockTargetFetcher {
	return &mockTargetFetcher{
		racks:                  make(map[uuid.UUID]*rack.Rack),
		racksByName:            make(map[string]*rack.Rack),
		domainRacks:            make(map[uuid.UUID][]*rack.Rack),
		domainRacksByName:      make(map[string][]*rack.Rack),
		components:             make(map[uuid.UUID]*component.Component),
		componentsByExternalID: make(map[string][]*component.Component),
	}
}

func (m *mockTargetFetcher) GetRacksForNVLDomain(
	_ context.Context,
	id identifier.Identifier,
) ([]*rack.Rack, error) {
	if m.getDomainErr != nil {
		return nil, m.getDomainErr
	}
	if id.ID != uuid.Nil {
		return m.domainRacks[id.ID], nil
	}
	return m.domainRacksByName[id.Name], nil
}

func (m *mockTargetFetcher) GetRackByIdentifier(
	ctx context.Context,
	id identifier.Identifier,
	withComponents bool,
) (*rack.Rack, error) {
	if m.getRackErr != nil {
		return nil, m.getRackErr
	}

	if id.ID != uuid.Nil {
		if r, ok := m.racks[id.ID]; ok {
			return r, nil
		}
	}

	if id.Name != "" {
		if r, ok := m.racksByName[id.Name]; ok {
			return r, nil
		}
	}

	return nil, nil
}

func (m *mockTargetFetcher) GetComponentByID(
	ctx context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	if m.getComponentErr != nil {
		return nil, m.getComponentErr
	}

	if c, ok := m.components[id]; ok {
		return c, nil
	}

	return nil, errors.New("component not found")
}

func (m *mockTargetFetcher) GetComponentByBMCMAC(
	_ context.Context,
	_ string,
) (*component.Component, error) {
	return nil, status.Error(codes.NotFound, "component not found")
}

func (m *mockTargetFetcher) GetComponentsByExternalIDs(
	ctx context.Context,
	externalIDs []string,
) ([]*component.Component, error) {
	if m.getExternalErr != nil {
		return nil, m.getExternalErr
	}

	var result []*component.Component
	for _, extID := range externalIDs {
		if comps, ok := m.componentsByExternalID[extID]; ok {
			result = append(result, comps...)
		}
	}

	return result, nil
}

func (m *mockTargetFetcher) addRack(r *rack.Rack) {
	m.racks[r.Info.ID] = r
	m.racksByName[r.Info.Name] = r
}

func (m *mockTargetFetcher) addComponent(c *component.Component) {
	m.components[c.Info.ID] = c
}

func (m *mockTargetFetcher) addComponentByExternalID(externalID string, c *component.Component) {
	m.componentsByExternalID[externalID] = append(m.componentsByExternalID[externalID], c)
}

// Helper functions to create test data
func newTestRack(id uuid.UUID, name string) *rack.Rack {
	info := deviceinfo.DeviceInfo{
		ID:           id,
		Name:         name,
		Manufacturer: "NVIDIA",
		SerialNumber: "SN-" + name,
	}
	loc := location.Location{
		Region:     "US",
		DataCenter: "DC1",
		Room:       "Room1",
		Position:   "Pos1",
	}
	return rack.New(info, loc)
}

func newTestComponent(id uuid.UUID, rackID uuid.UUID, compType devicetypes.ComponentType, name string) component.Component {
	comp := component.New(
		compType,
		&deviceinfo.DeviceInfo{
			ID:           id,
			Name:         name,
			Manufacturer: "NVIDIA",
			SerialNumber: "SN-" + name,
		},
		"1.0.0",
		&component.InRackPosition{SlotID: 1, TrayIndex: 0, HostID: 0},
	)

	comp.RackID = rackID
	return comp
}

func newTestComponentWithRackID(id uuid.UUID, rackID uuid.UUID, compType devicetypes.ComponentType, name string) *component.Component {
	c := newTestComponent(id, rackID, compType, name)
	c.RackID = rackID
	return &c
}

// Tests

func TestResolveTargetSpecToRacks_InvalidTargetSpec(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Empty target spec
	targetSpec := &operation.TargetSpec{}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	assert.Error(t, err)
	assert.Nil(t, result)
	assert.Contains(t, err.Error(), "invalid target spec")
}

func TestResolveTargetSpecToRacks_SingleRackTarget(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	comp1 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-1")
	comp2 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch, "comp-2")
	testRack.AddComponent(comp1)
	testRack.AddComponent(comp2)
	fetcher.addRack(testRack)

	// Target spec with rack ID
	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{ID: rackID}},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	assert.Contains(t, result, rackID)
	assert.Len(t, result[rackID].Components, 2)
}

func TestResolveTargetSpecToRacks_RackTargetByName(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	rackName := "my-rack"
	testRack := newTestRack(rackID, rackName)
	comp1 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-1")
	testRack.AddComponent(comp1)
	fetcher.addRack(testRack)

	// Target spec with rack name
	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{Name: rackName}},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	assert.Contains(t, result, rackID)
	assert.Len(t, result[rackID].Components, 1)
}

func TestResolveTargetSpecToRacks_RackTargetWithComponentTypeFilter(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	comp1 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-1")
	comp2 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch, "comp-2")
	comp3 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-3")
	testRack.AddComponent(comp1)
	testRack.AddComponent(comp2)
	testRack.AddComponent(comp3)
	fetcher.addRack(testRack)

	// Target spec filtering only Compute components
	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{
				Identifier:     identifier.Identifier{ID: rackID},
				ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeCompute},
			},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	assert.Len(t, result[rackID].Components, 2)
	for _, c := range result[rackID].Components {
		assert.Equal(t, devicetypes.ComponentTypeCompute, c.Type)
	}
}

func TestResolveTargetSpecToRacks_MultipleRackTargets(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rack1ID := uuid.New()
	rack2ID := uuid.New()
	testRack1 := newTestRack(rack1ID, "rack-1")
	testRack2 := newTestRack(rack2ID, "rack-2")
	comp1 := newTestComponent(uuid.New(), rack1ID, devicetypes.ComponentTypeCompute, "comp-1")
	comp2 := newTestComponent(uuid.New(), rack2ID, devicetypes.ComponentTypeCompute, "comp-2")
	testRack1.AddComponent(comp1)
	testRack2.AddComponent(comp2)
	fetcher.addRack(testRack1)
	fetcher.addRack(testRack2)

	// Target spec with multiple racks
	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{ID: rack1ID}},
			{Identifier: identifier.Identifier{ID: rack2ID}},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 2)
	assert.Contains(t, result, rack1ID)
	assert.Contains(t, result, rack2ID)
}

func TestResolveTargetSpecToRacks_DuplicateRackTargets_MergesComponents(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	comp1 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-1")
	comp2 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch, "comp-2")
	testRack.AddComponent(comp1)
	testRack.AddComponent(comp2)
	fetcher.addRack(testRack)

	// Target spec with same rack twice (different filters)
	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{
				Identifier:     identifier.Identifier{ID: rackID},
				ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeCompute},
			},
			{
				Identifier:     identifier.Identifier{ID: rackID},
				ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeNVSwitch},
			},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	// Both component types should be merged into the same rack
	assert.Len(t, result[rackID].Components, 2)
}

func TestResolveTargetSpecToRacks_RackNotFound(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// No racks added to fetcher
	nonExistentRackID := uuid.New()

	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{ID: nonExistentRackID}},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	assert.Error(t, err)
	assert.Nil(t, result)
	assert.Contains(t, err.Error(), "rack not found")
}

func TestResolveTargetSpecToRacks_RackFetchError(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()
	fetcher.getRackErr = errors.New("database connection failed")

	rackID := uuid.New()
	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{ID: rackID}},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	assert.Error(t, err)
	assert.Nil(t, result)
}

func TestResolveTargetSpecToRacks_RackFetchErrorTakesPrecedenceOverNilRack(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()
	fetcher.getRackErr = status.Error(codes.InvalidArgument, "rack name matches multiple racks; use rack id")

	targetSpec := &operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{Name: "shared"}},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.Error(t, err)
	assert.Nil(t, result)
	assert.Equal(t, codes.InvalidArgument, status.Code(err))
	assert.ErrorContains(t, err, "rack name matches multiple racks; use rack id")
	assert.NotContains(t, err.Error(), "rack not found")
}

func TestResolveTargetSpecToRacks_NVLinkDomainTargets(t *testing.T) {
	domainID := uuid.New()
	rackOneID := uuid.New()
	rackTwoID := uuid.New()
	computeID := uuid.New()
	nvSwitchID := uuid.New()
	rackOne := newTestRack(rackOneID, "rack-1")
	rackOne.AddComponent(newTestComponent(
		computeID,
		rackOneID,
		devicetypes.ComponentTypeCompute,
		"compute-1",
	))
	rackOne.AddComponent(newTestComponent(
		nvSwitchID,
		rackOneID,
		devicetypes.ComponentTypeNVSwitch,
		"nvswitch-1",
	))
	rackTwo := newTestRack(rackTwoID, "rack-2")
	rackTwo.AddComponent(newTestComponent(
		uuid.New(),
		rackTwoID,
		devicetypes.ComponentTypeCompute,
		"compute-2",
	))

	testCases := map[string]identifier.Identifier{
		"by ID":   {ID: domainID},
		"by name": {Name: "domain-1"},
	}
	for name, domainIdentifier := range testCases {
		t.Run(name, func(t *testing.T) {
			fetcher := newMockTargetFetcher()
			fetcher.addRack(rackOne)
			fetcher.addRack(rackTwo)
			fetcher.domainRacks[domainID] = []*rack.Rack{rackOne, rackTwo}
			fetcher.domainRacksByName["domain-1"] = []*rack.Rack{rackOne, rackTwo}

			result, err := resolveTargetSpecToRacks(
				context.Background(),
				fetcher,
				&operation.TargetSpec{
					NVLDomains: []operation.NVLDomainTarget{
						{
							Identifier: domainIdentifier,
							ComponentTypes: []devicetypes.ComponentType{
								devicetypes.ComponentTypeCompute,
							},
						},
					},
				},
			)
			require.NoError(t, err)
			require.Len(t, result, 2)
			require.Len(t, result[rackOneID].Components, 1)
			require.Equal(t, computeID, result[rackOneID].Components[0].Info.ID)
			require.Len(t, result[rackTwoID].Components, 1)
		})
	}
}

func TestResolveTargetSpecToRacks_ComponentTargetByUUID(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	compID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	testComp := newTestComponentWithRackID(compID, rackID, devicetypes.ComponentTypeCompute, "comp-1")
	fetcher.addRack(testRack)
	fetcher.addComponent(testComp)

	// Target spec with component UUID
	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{UUID: compID},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	assert.Contains(t, result, rackID)
	assert.Len(t, result[rackID].Components, 1)
	assert.Equal(t, compID, result[rackID].Components[0].Info.ID)
}

func TestResolveTargetSpecToRacks_ComponentTargetByExternalRef(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	compID := uuid.New()
	externalID := "machine-123"
	testRack := newTestRack(rackID, "rack-1")
	testComp := newTestComponentWithRackID(compID, rackID, devicetypes.ComponentTypeCompute, "comp-1")
	fetcher.addRack(testRack)
	fetcher.addComponentByExternalID(externalID, testComp)

	// Target spec with external reference
	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{
				External: &operation.ExternalRef{
					Type: devicetypes.ComponentTypeUnknown,
					ID:   externalID,
				},
			},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	assert.Contains(t, result, rackID)
	assert.Len(t, result[rackID].Components, 1)
}

func TestResolveTargetSpecToRacks_MultipleComponentTargets_SameRack(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rackID := uuid.New()
	comp1ID := uuid.New()
	comp2ID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	testComp1 := newTestComponentWithRackID(comp1ID, rackID, devicetypes.ComponentTypeCompute, "comp-1")
	testComp2 := newTestComponentWithRackID(comp2ID, rackID, devicetypes.ComponentTypeCompute, "comp-2")
	fetcher.addRack(testRack)
	fetcher.addComponent(testComp1)
	fetcher.addComponent(testComp2)

	// Target spec with multiple components from same rack
	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{UUID: comp1ID},
			{UUID: comp2ID},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 1)
	assert.Contains(t, result, rackID)
	assert.Len(t, result[rackID].Components, 2)
}

func TestResolveTargetSpecToRacks_MultipleComponentTargets_DifferentRacks(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup test data
	rack1ID := uuid.New()
	rack2ID := uuid.New()
	comp1ID := uuid.New()
	comp2ID := uuid.New()
	testRack1 := newTestRack(rack1ID, "rack-1")
	testRack2 := newTestRack(rack2ID, "rack-2")
	testComp1 := newTestComponentWithRackID(comp1ID, rack1ID, devicetypes.ComponentTypeCompute, "comp-1")
	testComp2 := newTestComponentWithRackID(comp2ID, rack2ID, devicetypes.ComponentTypeCompute, "comp-2")
	fetcher.addRack(testRack1)
	fetcher.addRack(testRack2)
	fetcher.addComponent(testComp1)
	fetcher.addComponent(testComp2)

	// Target spec with components from different racks
	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{UUID: comp1ID},
			{UUID: comp2ID},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	require.NoError(t, err)
	require.Len(t, result, 2)
	assert.Contains(t, result, rack1ID)
	assert.Contains(t, result, rack2ID)
	assert.Len(t, result[rack1ID].Components, 1)
	assert.Len(t, result[rack2ID].Components, 1)
}

func TestResolveTargetSpecToRacks_ComponentNotFound(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	nonExistentCompID := uuid.New()

	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{UUID: nonExistentCompID},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	assert.Error(t, err)
	assert.Nil(t, result)
}

func TestResolveTargetSpecToRacks_ExternalComponentNotFound(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{
				External: &operation.ExternalRef{
					Type: devicetypes.ComponentTypeCompute,
					ID:   "non-existent-id",
				},
			},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	assert.Error(t, err)
	assert.Nil(t, result)
	assert.Contains(t, err.Error(), "not found")
}

func TestResolveTargetSpecToRacks_ComponentFetchError(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()
	fetcher.getComponentErr = errors.New("database error")

	compID := uuid.New()
	targetSpec := &operation.TargetSpec{
		Components: []operation.ComponentTarget{
			{UUID: compID},
		},
	}

	result, err := resolveTargetSpecToRacks(ctx, fetcher, targetSpec)

	assert.Error(t, err)
	assert.Nil(t, result)
}

func TestFetchComponentTarget_InvalidTarget(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// ComponentTarget with neither UUID nor External set
	ct := &operation.ComponentTarget{}

	result, err := fetchComponentTarget(ctx, fetcher, ct)

	assert.Error(t, err)
	assert.Nil(t, result)
	assert.Contains(t, err.Error(), "invalid component target")
}

func TestResolveRackTarget_NoComponentTypeFilter_ReturnsAll(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup rack with multiple component types
	rackID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	comp1 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-1")
	comp2 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch, "comp-2")
	comp3 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypePowerShelf, "comp-3")
	testRack.AddComponent(comp1)
	testRack.AddComponent(comp2)
	testRack.AddComponent(comp3)
	fetcher.addRack(testRack)

	rt := &operation.RackTarget{
		Identifier:     identifier.Identifier{ID: rackID},
		ComponentTypes: nil, // No filter
	}

	result, err := resolveRackTarget(ctx, fetcher, rt)

	require.NoError(t, err)
	assert.Len(t, result.Components, 3)
}

func TestResolveRackTarget_MultipleComponentTypeFilters(t *testing.T) {
	ctx := context.Background()
	fetcher := newMockTargetFetcher()

	// Setup rack with multiple component types
	rackID := uuid.New()
	testRack := newTestRack(rackID, "rack-1")
	comp1 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "comp-1")
	comp2 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch, "comp-2")
	comp3 := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypePowerShelf, "comp-3")
	testRack.AddComponent(comp1)
	testRack.AddComponent(comp2)
	testRack.AddComponent(comp3)
	fetcher.addRack(testRack)

	rt := &operation.RackTarget{
		Identifier: identifier.Identifier{ID: rackID},
		ComponentTypes: []devicetypes.ComponentType{
			devicetypes.ComponentTypeCompute,
			devicetypes.ComponentTypeNVSwitch,
		},
	}

	result, err := resolveRackTarget(ctx, fetcher, rt)

	require.NoError(t, err)
	assert.Len(t, result.Components, 2)

	// Verify only the requested types are included
	for _, c := range result.Components {
		assert.True(t, c.Type == devicetypes.ComponentTypeCompute || c.Type == devicetypes.ComponentTypeNVSwitch)
	}
}
