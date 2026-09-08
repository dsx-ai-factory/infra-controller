// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"net"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	inventorymanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/manager"
	inventorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/store"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// --- Minimal mock for inventorymanager.Manager ---

type mockManager struct {
	inventorymanager.Manager // embed to satisfy the interface; unimplemented methods will panic

	components      map[uuid.UUID]*component.Component
	componentsByMAC map[string]*component.Component
	racks           map[uuid.UUID]*rack.Rack
	domainRacks     map[uuid.UUID][]*rack.Rack
	drifts          []inventorystore.ComponentDrift
}

func newMockManager() *mockManager {
	return &mockManager{
		components:      make(map[uuid.UUID]*component.Component),
		componentsByMAC: make(map[string]*component.Component),
		racks:           make(map[uuid.UUID]*rack.Rack),
		domainRacks:     make(map[uuid.UUID][]*rack.Rack),
	}
}

func (m *mockManager) GetRackByIdentifier(
	_ context.Context,
	id identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	if id.ExternalID != "" {
		for _, r := range m.racks {
			if r.ExternalID == id.ExternalID {
				return r, nil
			}
		}
		return nil, status.Error(codes.NotFound, "rack not found")
	}
	return m.GetRackByID(context.Background(), id.ID, true)
}

func (m *mockManager) GetRacksForNVLDomain(
	_ context.Context,
	id identifier.Identifier,
) ([]*rack.Rack, error) {
	return m.domainRacks[id.ID], nil
}

func (m *mockManager) GetDriftsByComponentIDs(_ context.Context, componentIDs []uuid.UUID) ([]inventorystore.ComponentDrift, error) {
	idSet := make(map[uuid.UUID]bool, len(componentIDs))
	for _, id := range componentIDs {
		idSet[id] = true
	}
	var result []inventorystore.ComponentDrift
	for _, d := range m.drifts {
		if d.ComponentID != nil && idSet[*d.ComponentID] {
			result = append(result, d)
		}
	}
	return result, nil
}

func (m *mockManager) GetAllDrifts(_ context.Context) ([]inventorystore.ComponentDrift, error) {
	return m.drifts, nil
}

func (m *mockManager) GetRackByID(_ context.Context, id uuid.UUID, _ bool) (*rack.Rack, error) {
	if r, ok := m.racks[id]; ok {
		return r, nil
	}
	return nil, assert.AnError
}

func (m *mockManager) GetRackByExternalID(
	_ context.Context,
	externalID string,
	_ bool,
) (*rack.Rack, error) {
	for _, r := range m.racks {
		if r.ExternalID == externalID {
			return r, nil
		}
	}
	return nil, status.Error(codes.NotFound, "rack not found")
}

func (m *mockManager) GetRacksByIDs(
	_ context.Context,
	ids []uuid.UUID,
	_ bool,
) ([]*rack.Rack, error) {
	result := make([]*rack.Rack, 0, len(ids))
	for _, id := range ids {
		if r, ok := m.racks[id]; ok {
			result = append(result, r)
		}
	}
	return result, nil
}

func (m *mockManager) GetRacksByIDsIncludingDeleted(
	ctx context.Context,
	ids []uuid.UUID,
	withComponents bool,
) ([]*rack.Rack, error) {
	return m.GetRacksByIDs(ctx, ids, withComponents)
}

func (m *mockManager) GetComponentByID(_ context.Context, id uuid.UUID) (*component.Component, error) {
	if c, ok := m.components[id]; ok {
		return c, nil
	}
	return nil, assert.AnError
}

func (m *mockManager) GetComponentsByExternalIDs(_ context.Context, externalIDs []string) ([]*component.Component, error) {
	lookup := make(map[string]bool, len(externalIDs))
	for _, id := range externalIDs {
		lookup[id] = true
	}
	var result []*component.Component
	for _, comp := range m.components {
		if comp.ComponentID != "" && lookup[comp.ComponentID] {
			result = append(result, comp)
		}
	}
	return result, nil
}

func (m *mockManager) GetComponentByBMCMAC(_ context.Context, macAddress string) (*component.Component, error) {
	if c, ok := m.componentsByMAC[macAddress]; ok {
		return c, nil
	}
	return nil, status.Error(codes.NotFound, "component not found")
}

func (m *mockManager) AddComponent(_ context.Context, comp *component.Component) (uuid.UUID, error) {
	m.components[comp.Info.ID] = comp
	return comp.Info.ID, nil
}

func (m *mockManager) PatchComponent(_ context.Context, comp *component.Component) error {
	m.components[comp.Info.ID] = comp
	return nil
}

func (m *mockManager) DeleteComponent(_ context.Context, id uuid.UUID) error {
	delete(m.components, id)
	return nil
}

func (m *mockManager) DeleteRack(_ context.Context, id uuid.UUID) error {
	if _, ok := m.racks[id]; !ok {
		return assert.AnError
	}
	delete(m.racks, id)
	return nil
}

func (m *mockManager) PurgeRack(_ context.Context, id uuid.UUID) error {
	if _, ok := m.racks[id]; !ok {
		return assert.AnError
	}
	delete(m.racks, id)
	return nil
}

func (m *mockManager) PurgeComponent(_ context.Context, id uuid.UUID) error {
	if _, ok := m.components[id]; !ok {
		return assert.AnError
	}
	delete(m.components, id)
	return nil
}

// --- Tests ---

func TestGetRackInfoByIDPrefersExternalID(t *testing.T) {
	mgr := newMockManager()
	internalID := uuid.New()
	mgr.racks[internalID] = &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: internalID, Name: "rack-1"},
		ExternalID: "core-rack-01",
	}

	response, err := (&FlowServerImpl{inventoryManager: mgr}).GetRackInfoByID(
		context.Background(),
		&pb.GetRackInfoByIDRequest{Id: &pb.UUID{Id: "core-rack-01"}},
	)

	require.NoError(t, err)
	require.NotNil(t, response.GetRack())
	assert.Equal(t, "core-rack-01", response.GetRack().GetExternalId())
}

func TestGetRackInfoByIDDoesNotResolveFlowUUID(t *testing.T) {
	mgr := newMockManager()
	internalID := uuid.New()
	mgr.racks[internalID] = &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: internalID, Name: "rack-1"},
		ExternalID: "core-rack-01",
	}

	response, err := (&FlowServerImpl{inventoryManager: mgr}).GetRackInfoByID(
		context.Background(),
		&pb.GetRackInfoByIDRequest{Id: &pb.UUID{Id: internalID.String()}},
	)

	require.Nil(t, response)
	assert.Equal(t, codes.NotFound, status.Code(err))
}

func TestGetComponentInfoByIDResolvesExternalIdentifier(t *testing.T) {
	coreID := "core-machine-01"
	internalID := uuid.New()
	resolved := &component.Component{
		Info:        deviceinfo.DeviceInfo{ID: internalID},
		Type:        devicetypes.ComponentTypeCompute,
		ComponentID: coreID,
	}

	tests := []struct {
		name       string
		identifier string
		setup      func(*mockManager)
		wantCode   codes.Code
	}{
		{
			name:       "external component ID resolves",
			identifier: coreID,
			setup: func(m *mockManager) {
				m.components[internalID] = resolved
			},
		},
		{
			name:       "normalized BMC MAC resolves",
			identifier: "AA-BB-CC-DD-EE-FF",
			setup: func(m *mockManager) {
				m.componentsByMAC["aa:bb:cc:dd:ee:ff"] = resolved
			},
		},
		{
			name:       "Flow UUID is not an external identifier",
			identifier: internalID.String(),
			setup: func(m *mockManager) {
				m.components[internalID] = resolved
			},
			wantCode: codes.NotFound,
		},
		{
			name:       "duplicate external ID across component types is rejected",
			identifier: coreID,
			setup: func(m *mockManager) {
				m.components[internalID] = resolved
				other := &component.Component{
					Info:        deviceinfo.DeviceInfo{ID: uuid.New()},
					Type:        devicetypes.ComponentTypeNVSwitch,
					ComponentID: coreID,
				}
				m.components[other.Info.ID] = other
			},
			wantCode: codes.FailedPrecondition,
		},
		{
			name:       "BMC MAC resolves despite duplicate external ID",
			identifier: "AA-BB-CC-DD-EE-FF",
			setup: func(m *mockManager) {
				m.components[internalID] = resolved
				other := &component.Component{
					Info:        deviceinfo.DeviceInfo{ID: uuid.New()},
					Type:        devicetypes.ComponentTypeNVSwitch,
					ComponentID: coreID,
				}
				m.components[other.Info.ID] = other
				m.componentsByMAC["aa:bb:cc:dd:ee:ff"] = resolved
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mgr := newMockManager()
			tt.setup(mgr)
			response, err := (&FlowServerImpl{inventoryManager: mgr}).GetComponentInfoByID(
				context.Background(),
				&pb.GetComponentInfoByIDRequest{Id: &pb.UUID{Id: tt.identifier}},
			)
			if tt.wantCode != codes.OK {
				require.Equal(t, tt.wantCode, status.Code(err))
				return
			}
			require.NoError(t, err)
			require.NotNil(t, response.GetComponent())
			assert.Equal(t, coreID, response.GetComponent().GetComponentId())
		})
	}
}

func TestAddComponent_Success(t *testing.T) {
	mgr := newMockManager()
	rackID := uuid.New()
	mgr.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID, Name: "test-rack"}}

	server := &FlowServerImpl{inventoryManager: mgr}

	req := &pb.AddComponentRequest{
		Component: &pb.Component{
			Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
			Info: &pb.DeviceInfo{
				Id:           &pb.UUID{Id: uuid.New().String()},
				Name:         "node-01",
				Manufacturer: "NVIDIA",
				SerialNumber: "SN123",
			},
			FirmwareVersion: "1.0.0",
			Position: &pb.RackPosition{
				SlotId:  1,
				TrayIdx: 0,
				HostId:  1,
			},
			RackId: &pb.UUID{Id: rackID.String()},
		},
	}

	resp, err := server.AddComponent(context.Background(), req)
	require.NoError(t, err)
	require.NotNil(t, resp)
	require.NotNil(t, resp.Component)
	assert.Equal(t, "node-01", resp.Component.Info.Name)
	assert.Equal(t, pb.ComponentType_COMPONENT_TYPE_COMPUTE, resp.Component.Type)
	assert.Equal(t, "1.0.0", resp.Component.FirmwareVersion)
	assert.Equal(t, int32(1), resp.Component.Position.SlotId)
}

// TestAddComponent_NoRackID verifies that a component can be ingested
// without a rack assignment. The component is stored with a nil RackID and
// no rack existence check is performed.
func TestAddComponent_NoRackID(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	compID := uuid.New()
	req := &pb.AddComponentRequest{
		Component: &pb.Component{
			Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
			Info: &pb.DeviceInfo{
				Id:           &pb.UUID{Id: compID.String()},
				Name:         "node-01",
				Manufacturer: "NVIDIA",
				SerialNumber: "SN123",
			},
			// rack_id intentionally not set
		},
	}

	resp, err := server.AddComponent(context.Background(), req)
	require.NoError(t, err)
	require.NotNil(t, resp)
	require.NotNil(t, resp.Component)
	assert.Equal(t, "node-01", resp.Component.Info.Name)

	// The component should be stored with RackID == uuid.Nil.
	stored, ok := mgr.components[compID]
	require.True(t, ok)
	assert.Equal(t, uuid.Nil, stored.RackID)
}

func TestAddComponent_MissingComponent(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	req := &pb.AddComponentRequest{
		// component not set
	}

	_, err := server.AddComponent(context.Background(), req)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "component is required")
}

func TestAddComponent_RackNotFound(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	req := &pb.AddComponentRequest{
		Component: &pb.Component{
			Type:   pb.ComponentType_COMPONENT_TYPE_COMPUTE,
			Info:   &pb.DeviceInfo{Name: "node-01", Manufacturer: "NVIDIA", SerialNumber: "SN123"},
			RackId: &pb.UUID{Id: uuid.New().String()}, // non-existent rack
		},
	}

	_, err := server.AddComponent(context.Background(), req)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "rack not found")
}

func TestDeleteComponent_Success(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	mgr.components[compID] = &component.Component{
		Type: devicetypes.ComponentTypeCompute,
		Info: deviceinfo.DeviceInfo{ID: compID, Name: "node-01"},
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.DeleteComponent(context.Background(), &pb.DeleteComponentRequest{
		Id: &pb.UUID{Id: compID.String()},
	})
	require.NoError(t, err)
	require.NotNil(t, resp)

	// Verify the component is removed from the mock
	_, exists := mgr.components[compID]
	assert.False(t, exists)
}

func TestDeleteComponent_MissingID(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.DeleteComponent(context.Background(), &pb.DeleteComponentRequest{})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "component id is required")
}

func TestDeleteComponent_NotFound(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.DeleteComponent(context.Background(), &pb.DeleteComponentRequest{
		Id: &pb.UUID{Id: uuid.New().String()},
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "component not found")
}

func TestPatchComponent_Success(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	rackID := uuid.New()
	mgr.components[compID] = &component.Component{
		Type: devicetypes.ComponentTypeCompute,
		Info: deviceinfo.DeviceInfo{ID: compID, Name: "node-01", Manufacturer: "NVIDIA", SerialNumber: "SN123"},
		Position: component.InRackPosition{
			SlotID:    1,
			TrayIndex: 0,
			HostID:    1,
		},
		FirmwareVersion: "1.0.0",
		RackID:          rackID,
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	newFW := "2.0.0"
	req := &pb.PatchComponentRequest{
		Id:              &pb.UUID{Id: compID.String()},
		FirmwareVersion: &newFW,
		Position: &pb.RackPosition{
			SlotId:  3,
			TrayIdx: 2,
			HostId:  5,
		},
	}

	resp, err := server.PatchComponent(context.Background(), req)
	require.NoError(t, err)
	require.NotNil(t, resp)
	require.NotNil(t, resp.Component)

	// Verify the update was persisted to mock
	updated := mgr.components[compID]
	assert.Equal(t, "2.0.0", updated.FirmwareVersion)
	assert.Equal(t, 3, updated.Position.SlotID)
	assert.Equal(t, 2, updated.Position.TrayIndex)
	assert.Equal(t, 5, updated.Position.HostID)
}

func TestPatchComponent_MissingID(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	newFW := "2.0.0"
	_, err := server.PatchComponent(context.Background(), &pb.PatchComponentRequest{
		FirmwareVersion: &newFW,
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "component id is required")
}

func TestPatchComponent_ComponentNotFound(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	newFW := "2.0.0"
	_, err := server.PatchComponent(context.Background(), &pb.PatchComponentRequest{
		Id:              &pb.UUID{Id: uuid.New().String()},
		FirmwareVersion: &newFW,
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to get component")
}

func TestPatchComponent_RackReassign(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	oldRackID := uuid.New()
	newRackID := uuid.New()
	mgr.racks[oldRackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: oldRackID, Name: "old-rack"}}
	mgr.racks[newRackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: newRackID, Name: "new-rack"}}
	mgr.components[compID] = &component.Component{
		Type:   devicetypes.ComponentTypeCompute,
		Info:   deviceinfo.DeviceInfo{ID: compID, Name: "node-01"},
		RackID: oldRackID,
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	req := &pb.PatchComponentRequest{
		Id:     &pb.UUID{Id: compID.String()},
		RackId: &pb.UUID{Id: newRackID.String()},
	}

	resp, err := server.PatchComponent(context.Background(), req)
	require.NoError(t, err)
	require.NotNil(t, resp)

	updated := mgr.components[compID]
	assert.Equal(t, newRackID, updated.RackID)
}

func TestPatchComponent_RackNotFound(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	rackID := uuid.New()
	mgr.components[compID] = &component.Component{
		Type:   devicetypes.ComponentTypeCompute,
		Info:   deviceinfo.DeviceInfo{ID: compID, Name: "node-01"},
		RackID: rackID,
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.PatchComponent(context.Background(), &pb.PatchComponentRequest{
		Id:     &pb.UUID{Id: compID.String()},
		RackId: &pb.UUID{Id: uuid.New().String()},
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "rack not found")
}

func TestPatchComponent_WithBMCs(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	rackID := uuid.New()
	mac, _ := net.ParseMAC("aa:bb:cc:dd:ee:ff")
	mgr.components[compID] = &component.Component{
		Type:   devicetypes.ComponentTypeCompute,
		Info:   deviceinfo.DeviceInfo{ID: compID, Name: "node-01"},
		RackID: rackID,
		BmcsByType: map[devicetypes.BMCType][]bmc.BMC{
			devicetypes.BMCTypeHost: {{MAC: bmc.MACAddress{HardwareAddr: mac}, IP: net.ParseIP("10.0.0.1")}},
		},
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	ip := "10.0.0.99"
	req := &pb.PatchComponentRequest{
		Id: &pb.UUID{Id: compID.String()},
		Bmcs: []*pb.BMCInfo{
			{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: "aa:bb:cc:dd:ee:ff",
				IpAddress:  &ip,
			},
		},
	}

	resp, err := server.PatchComponent(context.Background(), req)
	require.NoError(t, err)
	require.NotNil(t, resp)

	updated := mgr.components[compID]
	require.Len(t, updated.BmcsByType[devicetypes.BMCTypeHost], 1)
	assert.Equal(t, "10.0.0.99", updated.BmcsByType[devicetypes.BMCTypeHost][0].IP.String())
}

func TestPatchComponent_BMCsNotProvidedPreservesExisting(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	rackID := uuid.New()
	mac, _ := net.ParseMAC("aa:bb:cc:dd:ee:ff")
	mgr.components[compID] = &component.Component{
		Type:            devicetypes.ComponentTypeCompute,
		Info:            deviceinfo.DeviceInfo{ID: compID, Name: "node-01"},
		FirmwareVersion: "1.0.0",
		RackID:          rackID,
		BmcsByType: map[devicetypes.BMCType][]bmc.BMC{
			devicetypes.BMCTypeHost: {{MAC: bmc.MACAddress{HardwareAddr: mac}, IP: net.ParseIP("10.0.0.1")}},
		},
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	newFW := "2.0.0"
	req := &pb.PatchComponentRequest{
		Id:              &pb.UUID{Id: compID.String()},
		FirmwareVersion: &newFW,
	}

	resp, err := server.PatchComponent(context.Background(), req)
	require.NoError(t, err)
	require.NotNil(t, resp)

	updated := mgr.components[compID]
	assert.Equal(t, "2.0.0", updated.FirmwareVersion)
	require.Len(t, updated.BmcsByType[devicetypes.BMCTypeHost], 1)
	assert.Equal(t, "10.0.0.1", updated.BmcsByType[devicetypes.BMCTypeHost][0].IP.String())
}

// --- GetComponents Tests ---

func TestGetComponents_TargetSpecNoPagination(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.GetComponents(context.Background(), &pb.GetComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	assert.Equal(t, int32(3), resp.Total)
	assert.Equal(t, 3, len(resp.Components))
}

func TestGetComponents_TargetSpecWithPagination(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.GetComponents(context.Background(), &pb.GetComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Pagination: &pb.Pagination{Offset: 0, Limit: 2},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	assert.Equal(t, int32(3), resp.Total)
	assert.Equal(t, 2, len(resp.Components))
}

func TestGetComponents_NVLinkDomainTarget(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	domainID := uuid.New()
	mgr.domainRacks[domainID] = []*rack.Rack{mgr.racks[rackID]}
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.GetComponents(context.Background(), &pb.GetComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_NvlDomains{
				NvlDomains: &pb.NVLDomainTargets{
					Targets: []*pb.NVLDomainTarget{
						{
							Identifier: &pb.NVLDomainTarget_Id{
								Id: &pb.UUID{Id: domainID.String()},
							},
							ComponentTypes: []pb.ComponentType{
								pb.ComponentType_COMPONENT_TYPE_COMPUTE,
							},
						},
						{
							Identifier: &pb.NVLDomainTarget_Id{
								Id: &pb.UUID{Id: domainID.String()},
							},
							ComponentTypes: []pb.ComponentType{
								pb.ComponentType_COMPONENT_TYPE_COMPUTE,
							},
						},
					},
				},
			},
		},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	assert.Equal(t, int32(2), resp.Total)
	assert.Len(t, resp.Components, 2)
}

// --- ValidateComponents Tests ---

// helper to build a rack with components for validate tests
func setupValidateTestData(mgr *mockManager) (uuid.UUID, []uuid.UUID) {
	rackID := uuid.New()

	comp1ID := uuid.New()
	comp2ID := uuid.New()
	comp3ID := uuid.New()

	missingMAC, _ := net.ParseMAC("aa:bb:cc:dd:ee:02")

	mgr.racks[rackID] = &rack.Rack{
		Info: deviceinfo.DeviceInfo{ID: rackID, Name: "test-rack"},
		Components: []component.Component{
			{
				Type:            devicetypes.ComponentTypeCompute,
				Info:            deviceinfo.DeviceInfo{ID: comp1ID, Name: "compute-01", Manufacturer: "NVIDIA"},
				FirmwareVersion: "1.0.0",
				RackID:          rackID,
			},
			{
				Type:            devicetypes.ComponentTypeCompute,
				Info:            deviceinfo.DeviceInfo{ID: comp2ID, Name: "compute-02", Manufacturer: "NVIDIA"},
				FirmwareVersion: "1.0.0",
				RackID:          rackID,
				BmcsByType: map[devicetypes.BMCType][]bmc.BMC{
					devicetypes.BMCTypeHost: {{MAC: bmc.MACAddress{HardwareAddr: missingMAC}}},
				},
			},
			{
				Type:            devicetypes.ComponentTypeNVSwitch,
				Info:            deviceinfo.DeviceInfo{ID: comp3ID, Name: "nvswitch-01", Manufacturer: "Mellanox"},
				FirmwareVersion: "2.0.0",
				RackID:          rackID,
			},
		},
	}
	for i := range mgr.racks[rackID].Components {
		comp := &mgr.racks[rackID].Components[i]
		mgr.components[comp.Info.ID] = comp
	}

	// Set up drifts for comp1 (mismatch) and comp2 (missing_in_actual)
	mgr.drifts = []inventorystore.ComponentDrift{
		{
			ID:          uuid.New(),
			ComponentID: &comp1ID,
			DriftType:   "mismatch",
			Diffs: []inventorystore.FieldDiff{
				{FieldName: "firmware_version", ExpectedValue: "1.0.0", ActualValue: "1.1.0"},
			},
		},
		{
			ID:          uuid.New(),
			ComponentID: &comp2ID,
			DriftType:   "missing_in_actual",
		},
		{
			ID:          uuid.New(),
			ComponentID: &comp3ID,
			DriftType:   "mismatch",
			Diffs: []inventorystore.FieldDiff{
				{FieldName: "firmware_version", ExpectedValue: "2.0.0", ActualValue: "2.1.0"},
			},
		},
	}

	return rackID, []uuid.UUID{comp1ID, comp2ID, comp3ID}
}

func TestValidateComponents_NoFilters(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	// 3 drifts: comp1 mismatch, comp2 missing_in_actual, comp3 mismatch
	assert.Equal(t, int32(3), resp.TotalDiffs)
	assert.Equal(t, 3, len(resp.Diffs))
	assert.Equal(t, int32(2), resp.MismatchCount)
	assert.Equal(t, int32(1), resp.MissingCount)
	require.Equal(t, pb.DiffType_DIFF_TYPE_MISSING, resp.Diffs[1].GetType())
	assert.Nil(t, resp.Diffs[1].GetId())
	assert.Empty(t, resp.Diffs[1].GetComponentId())
	assert.Equal(t, "aa:bb:cc:dd:ee:02", resp.Diffs[1].GetComponentMacAddress())
}

func TestValidateComponents_WithTypeFilter(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Filters: []*pb.Filter{
			{
				Field:     &pb.Filter_ComponentField{ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE},
				QueryInfo: &pb.StringQueryInfo{Patterns: []string{"compute"}, IsWildcard: false, UseOr: false},
			},
		},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	// Only comp1 (mismatch) and comp2 (missing_in_actual) are compute; comp3 (nvswitch) is filtered out
	assert.Equal(t, int32(2), resp.TotalDiffs)
	assert.Equal(t, 2, len(resp.Diffs))
	assert.Equal(t, int32(1), resp.MismatchCount)
	assert.Equal(t, int32(1), resp.MissingCount)
}

func TestValidateComponents_WithNameFilter(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Filters: []*pb.Filter{
			{
				Field:     &pb.Filter_ComponentField{ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME},
				QueryInfo: &pb.StringQueryInfo{Patterns: []string{"compute-01"}, IsWildcard: false, UseOr: false},
			},
		},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	// Only comp1 (compute-01) matches the name filter
	assert.Equal(t, int32(1), resp.TotalDiffs)
	assert.Equal(t, 1, len(resp.Diffs))
	assert.Equal(t, int32(1), resp.MismatchCount) // comp1 is a mismatch
	assert.Equal(t, int32(0), resp.UnexpectedCount)
}

func TestValidateComponents_WithManufacturerFilter(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Filters: []*pb.Filter{
			{
				Field:     &pb.Filter_ComponentField{ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_MANUFACTURER},
				QueryInfo: &pb.StringQueryInfo{Patterns: []string{"Mellanox"}, IsWildcard: false, UseOr: false},
			},
		},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	// Only comp3 (nvswitch-01, Mellanox) matches
	assert.Equal(t, int32(1), resp.TotalDiffs)
	assert.Equal(t, int32(1), resp.MismatchCount) // comp3 is a mismatch
}

func TestValidateComponents_WithPagination(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	// Get first page (limit 2)
	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Pagination: &pb.Pagination{Offset: 0, Limit: 2},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	assert.Equal(t, int32(3), resp.TotalDiffs) // total is still 3
	assert.Equal(t, 2, len(resp.Diffs))        // but only 2 returned

	// Get second page (offset 2, limit 2)
	resp2, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Pagination: &pb.Pagination{Offset: 2, Limit: 2},
	})

	require.NoError(t, err)
	require.NotNil(t, resp2)
	assert.Equal(t, int32(3), resp2.TotalDiffs) // total is still 3
	assert.Equal(t, 1, len(resp2.Diffs))        // only 1 remaining
}

func TestValidateComponents_NoTargetSpec_GetAllDrifts(t *testing.T) {
	mgr := newMockManager()
	_, _ = setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	// No target_spec => GetAllDrifts
	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{})

	require.NoError(t, err)
	require.NotNil(t, resp)
	assert.Equal(t, int32(3), resp.TotalDiffs)
	assert.Equal(t, 3, len(resp.Diffs))
}

func TestValidateComponents_FilterAndPaginationCombined(t *testing.T) {
	mgr := newMockManager()
	rackID, _ := setupValidateTestData(mgr)
	server := &FlowServerImpl{inventoryManager: mgr}

	// Filter to compute only (2 drifts), then paginate (limit 1)
	resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: []*pb.RackTarget{
						{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}},
					},
				},
			},
		},
		Filters: []*pb.Filter{
			{
				Field:     &pb.Filter_ComponentField{ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE},
				QueryInfo: &pb.StringQueryInfo{Patterns: []string{"compute"}, IsWildcard: false, UseOr: false},
			},
		},
		Pagination: &pb.Pagination{Offset: 0, Limit: 1},
	})

	require.NoError(t, err)
	require.NotNil(t, resp)
	assert.Equal(t, int32(2), resp.TotalDiffs) // 2 compute drifts total
	assert.Equal(t, 1, len(resp.Diffs))        // but only 1 returned (paginated)
}

// --- DeleteRack Tests ---

func TestDeleteRack_Success(t *testing.T) {
	mgr := newMockManager()
	rackID := uuid.New()
	mgr.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID, Name: "test-rack"}}

	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.DeleteRack(context.Background(), &pb.DeleteRackRequest{
		Id: &pb.UUID{Id: rackID.String()},
	})
	require.NoError(t, err)
	require.NotNil(t, resp)

	_, exists := mgr.racks[rackID]
	assert.False(t, exists)
}

func TestDeleteRack_MissingID(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.DeleteRack(context.Background(), &pb.DeleteRackRequest{})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "rack id is required")
}

func TestDeleteRack_NotFound(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.DeleteRack(context.Background(), &pb.DeleteRackRequest{
		Id: &pb.UUID{Id: uuid.New().String()},
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to delete rack")
}

// --- PurgeRack Tests ---

func TestPurgeRack_Success(t *testing.T) {
	mgr := newMockManager()
	rackID := uuid.New()
	mgr.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID, Name: "test-rack"}}

	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.PurgeRack(context.Background(), &pb.PurgeRackRequest{
		Id: &pb.UUID{Id: rackID.String()},
	})
	require.NoError(t, err)
	require.NotNil(t, resp)

	_, exists := mgr.racks[rackID]
	assert.False(t, exists)
}

func TestPurgeRack_MissingID(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.PurgeRack(context.Background(), &pb.PurgeRackRequest{})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "rack id is required")
}

func TestPurgeRack_NotFound(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.PurgeRack(context.Background(), &pb.PurgeRackRequest{
		Id: &pb.UUID{Id: uuid.New().String()},
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to purge rack")
}

// --- PurgeComponent Tests ---

func TestPurgeComponent_Success(t *testing.T) {
	mgr := newMockManager()
	compID := uuid.New()
	mgr.components[compID] = &component.Component{
		Type: devicetypes.ComponentTypeCompute,
		Info: deviceinfo.DeviceInfo{ID: compID, Name: "node-01"},
	}

	server := &FlowServerImpl{inventoryManager: mgr}

	resp, err := server.PurgeComponent(context.Background(), &pb.PurgeComponentRequest{
		Id: &pb.UUID{Id: compID.String()},
	})
	require.NoError(t, err)
	require.NotNil(t, resp)

	_, exists := mgr.components[compID]
	assert.False(t, exists)
}

func TestPurgeComponent_MissingID(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.PurgeComponent(context.Background(), &pb.PurgeComponentRequest{})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "component id is required")
}

func TestPurgeComponent_NotFound(t *testing.T) {
	mgr := newMockManager()
	server := &FlowServerImpl{inventoryManager: mgr}

	_, err := server.PurgeComponent(context.Background(), &pb.PurgeComponentRequest{
		Id: &pb.UUID{Id: uuid.New().String()},
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to purge component")
}
