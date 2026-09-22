// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPITrayJSONContract(t *testing.T) {
	apiTray := NewAPITray(&flowv1.Component{})
	data, err := json.Marshal(apiTray)
	require.NoError(t, err)

	var got map[string]any
	require.NoError(t, json.Unmarshal(data, &got))
	value, ok := got["nvLinkDomainId"]
	assert.True(t, ok)
	assert.Nil(t, value)
	assert.Equal(t, map[string]any{
		"pendingTaskCount": float64(0),
		"activeTaskCount":  float64(0),
	}, got["taskStats"])
}

func TestProtoToAPIComponentTypeName(t *testing.T) {
	tests := []struct {
		name string
		ct   flowv1.ComponentType
		want string
	}{
		{
			name: "compute type",
			ct:   flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
			want: "Compute",
		},
		{
			name: "nvswitch type",
			ct:   flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH,
			want: "NVSwitch",
		},
		{
			name: "powershelf type",
			ct:   flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF,
			want: "PowerShelf",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ProtoToAPIComponentTypeName[tt.ct]
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestNewAPITray(t *testing.T) {
	description := "Test tray description"
	model := "GB200"
	domainID := "59202b81-65fb-45ec-b3b8-91ab0ad3f34a"

	tests := []struct {
		name string
		comp *flowv1.Component
		want *APITray
	}{
		{
			name: "nil component returns nil",
			comp: nil,
			want: nil,
		},
		{
			name: "basic compute tray",
			comp: &flowv1.Component{
				Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
				Info: &flowv1.DeviceInfo{
					Id:           &flowv1.UUID{Id: "tray-id-123"},
					Name:         "compute-tray-1",
					Manufacturer: "NVIDIA",
					Model:        &model,
					SerialNumber: "TSN001",
					Description:  &description,
				},
				FirmwareVersion: "2.1.0",
				ComponentId:     "nico-machine-456",
				RackExternalId:  "core-rack-789",
				Position: &flowv1.RackPosition{
					SlotId:  1,
					TrayIdx: 0,
					HostId:  1,
				},
				Bmcs: []*flowv1.BMCInfo{
					{
						Type:       flowv1.BMCType_BMC_TYPE_HOST,
						MacAddress: "00:11:22:33:44:55",
						IpAddress:  cutil.GetPtr("192.168.1.100"),
					},
				},
				RackId:      &flowv1.UUID{Id: "flow-rack-uuid"},
				NvlDomainId: &flowv1.UUID{Id: domainID},
			},
			want: &APITray{
				ID:              "nico-machine-456",
				Type:            "Compute",
				Name:            "compute-tray-1",
				Manufacturer:    "NVIDIA",
				Model:           "GB200",
				SerialNumber:    "TSN001",
				Description:     "Test tray description",
				FirmwareVersion: "2.1.0",
				Position: &APITrayPosition{
					SlotID:  1,
					TrayIdx: 0,
					HostID:  1,
				},
				BMCs: []*APIBMC{
					{
						Type:       "BmcTypeHost",
						MacAddress: "00:11:22:33:44:55",
						IPAddress:  "192.168.1.100",
					},
				},
				RackID:         "core-rack-789",
				NVLinkDomainID: cutil.GetPtr(domainID),
			},
		},
		{
			name: "switch tray without optional fields",
			comp: &flowv1.Component{
				Type:        flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH,
				ComponentId: "core-switch-1",
				Info: &flowv1.DeviceInfo{
					Id:           &flowv1.UUID{Id: "switch-tray-id"},
					Name:         "switch-tray-1",
					Manufacturer: "NVIDIA",
					SerialNumber: "SSN001",
				},
				FirmwareVersion: "1.5.0",
				Position: &flowv1.RackPosition{
					SlotId:  24,
					TrayIdx: 1,
				},
			},
			want: &APITray{
				ID:              "core-switch-1",
				Type:            "NVSwitch",
				Name:            "switch-tray-1",
				Manufacturer:    "NVIDIA",
				SerialNumber:    "SSN001",
				FirmwareVersion: "1.5.0",
				Position: &APITrayPosition{
					SlotID:  24,
					TrayIdx: 1,
					HostID:  0,
				},
			},
		},
		{
			name: "powershelf tray",
			comp: &flowv1.Component{
				Type:           flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF,
				ComponentId:    "core-powershelf-1",
				RackExternalId: "core-rack-abc",
				Info: &flowv1.DeviceInfo{
					Id:           &flowv1.UUID{Id: "power-tray-id"},
					Name:         "powershelf-1",
					Manufacturer: "NVIDIA",
					SerialNumber: "PSN001",
				},
				Position: &flowv1.RackPosition{
					SlotId: 48,
				},
				RackId: &flowv1.UUID{Id: "flow-rack-uuid"},
			},
			want: &APITray{
				ID:           "core-powershelf-1",
				Type:         "PowerShelf",
				Name:         "powershelf-1",
				Manufacturer: "NVIDIA",
				SerialNumber: "PSN001",
				Position: &APITrayPosition{
					SlotID:  48,
					TrayIdx: 0,
					HostID:  0,
				},
				RackID: "core-rack-abc",
			},
		},
		{
			name: "tray with minimal info and explicit type",
			comp: &flowv1.Component{
				Type:        flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
				ComponentId: "core-machine-minimal",
				Info: &flowv1.DeviceInfo{
					Id: &flowv1.UUID{Id: "minimal-tray"},
				},
			},
			want: &APITray{
				ID:   "core-machine-minimal",
				Type: "Compute",
			},
		},
		{
			name: "tray with unspecified type falls back to unknown",
			comp: &flowv1.Component{
				ComponentId: "core-component-untyped",
				Info: &flowv1.DeviceInfo{
					Id: &flowv1.UUID{Id: "untyped-tray"},
				},
			},
			want: &APITray{
				ID:   "core-component-untyped",
				Type: "Unknown",
			},
		},
		{
			name: "tray without info",
			comp: &flowv1.Component{
				Type:        flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
				ComponentId: "compute-component-123",
			},
			want: &APITray{
				ID:   "compute-component-123",
				Type: "Compute",
			},
		},
		{
			name: "tray without position",
			comp: &flowv1.Component{
				Type:        flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH,
				ComponentId: "core-switch-no-position",
				Info: &flowv1.DeviceInfo{
					Id:   &flowv1.UUID{Id: "switch-tray-id"},
					Name: "switch-1",
				},
			},
			want: &APITray{
				ID:       "core-switch-no-position",
				Type:     "NVSwitch",
				Name:     "switch-1",
				Position: nil,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPITray(tt.comp)

			if tt.want == nil {
				assert.Nil(t, got)
				return
			}

			assert.NotNil(t, got)
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Type, got.Type)
			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, tt.want.Manufacturer, got.Manufacturer)
			assert.Equal(t, tt.want.Model, got.Model)
			assert.Equal(t, tt.want.SerialNumber, got.SerialNumber)
			assert.Equal(t, tt.want.Description, got.Description)
			assert.Equal(t, tt.want.FirmwareVersion, got.FirmwareVersion)
			assert.Equal(t, tt.want.RackID, got.RackID)
			assert.Equal(t, tt.want.NVLinkDomainID, got.NVLinkDomainID)

			// Assert BMCs field
			if tt.want.BMCs != nil {
				require.NotNil(t, got.BMCs)
				assert.Len(t, got.BMCs, len(tt.want.BMCs))
				for i, wantBMC := range tt.want.BMCs {
					gotBMC := got.BMCs[i]
					assert.Equal(t, wantBMC.Type, gotBMC.Type, "BMC Type mismatch at index %d", i)
					assert.Equal(t, wantBMC.MacAddress, gotBMC.MacAddress, "BMC MacAddress mismatch at index %d", i)
					assert.Equal(t, wantBMC.IPAddress, gotBMC.IPAddress, "BMC IPAddress mismatch at index %d", i)
				}
			} else {
				assert.Nil(t, got.BMCs)
			}

			if tt.want.Position != nil {
				assert.NotNil(t, got.Position)
				assert.Equal(t, tt.want.Position.SlotID, got.Position.SlotID)
				assert.Equal(t, tt.want.Position.TrayIdx, got.Position.TrayIdx)
				assert.Equal(t, tt.want.Position.HostID, got.Position.HostID)
			} else {
				assert.Nil(t, got.Position)
			}
		})
	}
}

func TestAPITrayPosition_FromProto(t *testing.T) {
	pos := &APITrayPosition{}
	pos.FromProto(&flowv1.RackPosition{SlotId: 2, TrayIdx: 1, HostId: 0})
	assert.Equal(t, int32(2), pos.SlotID)
	assert.Equal(t, int32(1), pos.TrayIdx)
	assert.Equal(t, int32(0), pos.HostID)

	pos.FromProto(nil) // no-op
	assert.Equal(t, int32(2), pos.SlotID)
}

func TestAPITray_FromProto(t *testing.T) {
	comp := &flowv1.Component{
		Type:            flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
		ComponentId:     "comp-1",
		FirmwareVersion: "1.0",
		Info: &flowv1.DeviceInfo{
			Id:   &flowv1.UUID{Id: "tray-uuid"},
			Name: "My Tray",
		},
		Position:       &flowv1.RackPosition{SlotId: 3, TrayIdx: 0, HostId: 1},
		RackId:         &flowv1.UUID{Id: "rack-uuid"},
		RackExternalId: "core-rack-1",
		Status:         &flowv1.ComponentOperationStatus{Phase: flowv1.Phase_PHASE_IN_USE},
		LeakStatus:     flowv1.LeakStatus_LEAK_STATUS_DETECTED,
	}
	at := &APITray{}
	at.FromProto(comp)
	assert.Equal(t, "Compute", at.Type)
	assert.Equal(t, "comp-1", at.ID)
	assert.Equal(t, "My Tray", at.Name)
	assert.Equal(t, "core-rack-1", at.RackID)
	assert.Equal(t, "InUse", at.OperationStatus)
	assert.Equal(t, "Leaking", at.LeakStatus)
	assert.NotNil(t, at.Position)
	assert.Equal(t, int32(3), at.Position.SlotID)
	assert.Equal(t, int32(0), at.Position.TrayIdx)
	assert.Equal(t, int32(1), at.Position.HostID)

	at.FromProto(nil) // no-op, fields unchanged
	assert.Equal(t, "comp-1", at.ID)

	// A component with no computed operation status and no leak status
	// resolves both fields to "Unknown".
	bare := &APITray{}
	bare.FromProto(&flowv1.Component{Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE})
	assert.Equal(t, "Unknown", bare.OperationStatus)
	assert.Equal(t, "Unknown", bare.LeakStatus)
}

func TestAPITrayGetAllRequest_Validate(t *testing.T) {
	validUUID := uuid.New().String()
	validUUID2 := uuid.New().String()
	strPtr := func(s string) *string { return &s }
	int32Ptr := func(v int32) *int32 { return &v }

	tests := []struct {
		name    string
		req     APITrayGetAllRequest
		wantErr bool
		check   func(t *testing.T, req *APITrayGetAllRequest)
	}{
		{
			name:    "empty request is valid",
			req:     APITrayGetAllRequest{},
			wantErr: false,
		},
		{
			name:    "valid rackId only",
			req:     APITrayGetAllRequest{RackID: strPtr(validUUID)},
			wantErr: false,
		},
		{
			name:    "valid rackName only",
			req:     APITrayGetAllRequest{RackName: strPtr("Rack-001")},
			wantErr: false,
		},
		{
			name:    "non-UUID external rack ID is valid",
			req:     APITrayGetAllRequest{RackID: strPtr("core-rack-01")},
			wantErr: false,
		},
		{
			name:    "rackId and rackName mutually exclusive",
			req:     APITrayGetAllRequest{RackID: strPtr(validUUID), RackName: strPtr("Rack-001")},
			wantErr: true,
		},
		{
			name:    "valid type - Compute",
			req:     APITrayGetAllRequest{Type: strPtr("Compute")},
			wantErr: false,
		},
		{
			name:    "valid type - NVSwitch",
			req:     APITrayGetAllRequest{Type: strPtr("NVSwitch")},
			wantErr: false,
		},
		{
			name:    "valid type - PowerShelf",
			req:     APITrayGetAllRequest{Type: strPtr("PowerShelf")},
			wantErr: false,
		},
		{
			name:    "invalid type",
			req:     APITrayGetAllRequest{Type: strPtr("invalid-type")},
			wantErr: true,
		},
		{
			name:    "unsupported type - torswitch",
			req:     APITrayGetAllRequest{Type: strPtr("torswitch")},
			wantErr: true,
		},
		{
			name:    "unsupported type - ums",
			req:     APITrayGetAllRequest{Type: strPtr("ums")},
			wantErr: true,
		},
		{
			name:    "unsupported type - cdu",
			req:     APITrayGetAllRequest{Type: strPtr("cdu")},
			wantErr: true,
		},
		{
			name:    "valid IDs",
			req:     APITrayGetAllRequest{IDs: []string{validUUID, validUUID2}},
			wantErr: false,
		},
		{
			name:    "non-UUID component ID is valid",
			req:     APITrayGetAllRequest{IDs: []string{"core-machine-1"}},
			wantErr: false,
		},
		{
			name:    "IDs with type are valid",
			req:     APITrayGetAllRequest{IDs: []string{"comp-1", "comp-2"}, Type: strPtr("Compute")},
			wantErr: false,
		},
		{
			name:    "rackId conflicts with IDs",
			req:     APITrayGetAllRequest{RackID: strPtr(validUUID), IDs: []string{validUUID2}},
			wantErr: true,
		},
		{
			name:    "rackName conflicts with IDs",
			req:     APITrayGetAllRequest{RackName: strPtr("Rack-001"), IDs: []string{validUUID}},
			wantErr: true,
		},
		{
			name:    "rackId with type is valid (rack-level)",
			req:     APITrayGetAllRequest{RackID: strPtr(validUUID), Type: strPtr("Compute")},
			wantErr: false,
		},
		{
			name:    "slotId with rackName is valid",
			req:     APITrayGetAllRequest{RackName: strPtr("Rack-001"), SlotID: int32Ptr(3)},
			wantErr: false,
		},
		{
			name:    "slotId with rackId is valid",
			req:     APITrayGetAllRequest{RackID: strPtr(validUUID), SlotID: int32Ptr(3)},
			wantErr: false,
		},
		{
			name:    "slotId without rack invalid",
			req:     APITrayGetAllRequest{SlotID: int32Ptr(3)},
			wantErr: true,
		},
		{
			name:    "slotId with IDs without rack invalid",
			req:     APITrayGetAllRequest{SlotID: int32Ptr(3), IDs: []string{validUUID}},
			wantErr: true,
		},
		{
			name:    "negative slotId invalid",
			req:     APITrayGetAllRequest{RackName: strPtr("Rack-001"), SlotID: int32Ptr(-1)},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
			if tt.check != nil {
				tt.check(t, &tt.req)
			}
		})
	}
}

func TestAPITrayGetAllRequest_ToProto(t *testing.T) {
	rackID := "core-rack-01"
	rackName := "Rack-001"
	trayType := "Compute"
	id1 := "core-machine-01"
	id2 := "core-machine-02"

	tests := []struct {
		name     string
		request  *APITrayGetAllRequest
		validate func(t *testing.T, req *flowv1.GetComponentsRequest)
	}{
		{
			name:    "empty request - no TargetSpec, queries all components",
			request: &APITrayGetAllRequest{},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				assert.Nil(t, req.TargetSpec)
				assert.Empty(t, req.Filters)
			},
		},
		{
			name: "rackId only - rack-level targeting with supported types",
			request: &APITrayGetAllRequest{
				RackID: &rackID,
			},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				require.NotNil(t, req.TargetSpec)
				rackTargets := req.TargetSpec.GetRacks()
				require.NotNil(t, rackTargets)
				require.Len(t, rackTargets.Targets, 1)
				assert.Equal(t, rackID, rackTargets.Targets[0].GetExternalId())
				assert.ElementsMatch(t, ValidProtoComponentTypes, rackTargets.Targets[0].ComponentTypes)
			},
		},
		{
			name: "rackName only - rack-level targeting with supported types",
			request: &APITrayGetAllRequest{
				RackName: &rackName,
			},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				require.NotNil(t, req.TargetSpec)
				rackTargets := req.TargetSpec.GetRacks()
				require.NotNil(t, rackTargets)
				require.Len(t, rackTargets.Targets, 1)
				assert.Equal(t, rackName, rackTargets.Targets[0].GetName())
				assert.ElementsMatch(t, ValidProtoComponentTypes, rackTargets.Targets[0].ComponentTypes)
			},
		},
		{
			name: "type only - no TargetSpec, type passed as filter",
			request: &APITrayGetAllRequest{
				Type: &trayType,
			},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				assert.Nil(t, req.TargetSpec)
				require.Len(t, req.Filters, 1)
				assert.Equal(t, flowv1.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE, req.Filters[0].GetComponentField())
				assert.Contains(t, req.Filters[0].GetQueryInfo().GetPatterns(), "Compute")
			},
		},
		{
			name: "rackId with type - rack-level targeting with filter",
			request: &APITrayGetAllRequest{
				RackID: &rackID,
				Type:   &trayType,
			},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				require.NotNil(t, req.TargetSpec)
				rackTargets := req.TargetSpec.GetRacks()
				require.NotNil(t, rackTargets)
				require.Len(t, rackTargets.Targets, 1)
				assert.Equal(t, rackID, rackTargets.Targets[0].GetExternalId())
				assert.Contains(t, rackTargets.Targets[0].ComponentTypes, flowv1.ComponentType_COMPONENT_TYPE_COMPUTE)
			},
		},
		{
			name: "IDs - component-level targeting",
			request: &APITrayGetAllRequest{
				IDs: []string{id1, id2},
			},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				require.NotNil(t, req.TargetSpec)
				compTargets := req.TargetSpec.GetComponents()
				require.NotNil(t, compTargets)
				assert.Len(t, compTargets.Targets, 2)
				assert.Equal(t, id1, compTargets.Targets[0].GetExternal().GetId())
				assert.Equal(t, id2, compTargets.Targets[1].GetExternal().GetId())
				assert.Equal(t, flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN, compTargets.Targets[0].GetExternal().GetType())
			},
		},
		{
			name: "IDs with type - typed component-level targeting",
			request: &APITrayGetAllRequest{
				IDs:  []string{"comp-1", "comp-2"},
				Type: &trayType,
			},
			validate: func(t *testing.T, req *flowv1.GetComponentsRequest) {
				require.NotNil(t, req.TargetSpec)
				compTargets := req.TargetSpec.GetComponents()
				require.NotNil(t, compTargets)
				assert.Len(t, compTargets.Targets, 2)
				for _, target := range compTargets.Targets {
					ext := target.GetExternal()
					require.NotNil(t, ext)
					assert.Equal(t, flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, ext.Type)
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := tt.request.ToProto()
			require.NotNil(t, req)
			if tt.validate != nil {
				tt.validate(t, req)
			}
		})
	}
}

func TestTrayFilter_ToTargetSpec(t *testing.T) {
	t.Run("component ID and type", func(t *testing.T) {
		trayType := "Compute"
		filter := &TrayFilter{IDs: []string{"machine-1"}, Type: &trayType}

		targets := filter.ToTargetSpec().GetComponents().GetTargets()
		require.Len(t, targets, 1)
		assert.Equal(t, "machine-1", targets[0].GetExternal().GetId())
		assert.Equal(t, flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, targets[0].GetExternal().GetType())
	})

	t.Run("rack ID", func(t *testing.T) {
		rackID := "rack-1"
		filter := &TrayFilter{RackID: &rackID}

		targets := filter.ToTargetSpec().GetRacks().GetTargets()
		require.Len(t, targets, 1)
		assert.Equal(t, rackID, targets[0].GetExternalId())
		assert.Nil(t, targets[0].GetId())
	})
}

func TestTrayFilter_SlotFilter(t *testing.T) {
	rackName := "Rack-001"
	int32Ptr := func(v int32) *int32 { return &v }

	t.Run("nil filter has no slot filter and matches everything", func(t *testing.T) {
		var f *TrayFilter
		assert.False(t, f.HasSlotFilter())
		assert.True(t, f.MatchesSlot(&flowv1.Component{}))
	})

	t.Run("filter without slot has no slot filter", func(t *testing.T) {
		f := &TrayFilter{RackName: &rackName}
		assert.False(t, f.HasSlotFilter())
		assert.True(t, f.MatchesSlot(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 99},
		}))
	})

	t.Run("slotId matches position.slotId", func(t *testing.T) {
		f := &TrayFilter{RackName: &rackName, SlotID: int32Ptr(3)}
		assert.True(t, f.HasSlotFilter())
		assert.True(t, f.MatchesSlot(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 3, TrayIdx: 7},
		}))
		assert.False(t, f.MatchesSlot(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 4},
		}))
	})

	t.Run("slot filter rejects component with no position", func(t *testing.T) {
		f := &TrayFilter{RackName: &rackName, SlotID: int32Ptr(3)}
		assert.False(t, f.MatchesSlot(&flowv1.Component{}))
	})
}

func TestRackComponentSlotMatcher(t *testing.T) {
	int32Ptr := func(v int32) *int32 { return &v }

	t.Run("inactive matcher matches everything", func(t *testing.T) {
		m := RackComponentSlotMatcher{}
		assert.False(t, m.Active())
		assert.True(t, m.Matches(nil))
	})

	t.Run("active matcher checks slot", func(t *testing.T) {
		m := RackComponentSlotMatcher{SlotID: int32Ptr(3)}
		assert.True(t, m.Active())
		assert.True(t, m.Matches(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 3},
		}))
		assert.False(t, m.Matches(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 4},
		}))
		assert.False(t, m.Matches(nil))
	})
}

func TestTrayFilter_Validate_SlotConstraints(t *testing.T) {
	rackName := "Rack-001"
	int32Ptr := func(v int32) *int32 { return &v }

	tests := []struct {
		name    string
		filter  *TrayFilter
		wantErr bool
	}{
		{
			name:    "slotId with rackName valid",
			filter:  &TrayFilter{RackName: &rackName, SlotID: int32Ptr(3)},
			wantErr: false,
		},
		{
			name:    "slotId without rack invalid",
			filter:  &TrayFilter{SlotID: int32Ptr(3)},
			wantErr: true,
		},
		{
			name:    "slotId with ids without rack invalid",
			filter:  &TrayFilter{SlotID: int32Ptr(3), IDs: []string{uuid.New().String()}},
			wantErr: true,
		},
		{
			name:    "rackName with ids still invalid (pre-existing rule)",
			filter:  &TrayFilter{RackName: &rackName, IDs: []string{uuid.New().String()}},
			wantErr: true,
		},
		{
			name:    "negative slotId invalid",
			filter:  &TrayFilter{RackName: &rackName, SlotID: int32Ptr(-1)},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.filter.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestAPITrayValidateAllRequest_Validate_SlotConstraints(t *testing.T) {
	validUUID := uuid.New().String()
	rackName := "Rack-001"
	int32Ptr := func(v int32) *int32 { return &v }

	tests := []struct {
		name    string
		req     APITrayValidateAllRequest
		wantErr bool
	}{
		{
			name:    "slotId with rackName valid",
			req:     APITrayValidateAllRequest{SiteID: validUUID, RackName: &rackName, SlotID: int32Ptr(3)},
			wantErr: false,
		},
		{
			name:    "slotId without rack invalid",
			req:     APITrayValidateAllRequest{SiteID: validUUID, SlotID: int32Ptr(3)},
			wantErr: true,
		},
		{
			name:    "negative slotId invalid",
			req:     APITrayValidateAllRequest{SiteID: validUUID, RackName: &rackName, SlotID: int32Ptr(-1)},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestAPITrayValidateAllRequest_ToTargetSpec(t *testing.T) {
	t.Run("component ID", func(t *testing.T) {
		request := &APITrayValidateAllRequest{IDs: []string{"switch-1"}}

		targets := request.ToTargetSpec().GetComponents().GetTargets()
		require.Len(t, targets, 1)
		assert.Equal(t, "switch-1", targets[0].GetExternal().GetId())
		assert.Equal(t, flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN, targets[0].GetExternal().GetType())
	})

	t.Run("rack ID", func(t *testing.T) {
		rackID := "rack-1"
		request := &APITrayValidateAllRequest{RackID: &rackID}

		targets := request.ToTargetSpec().GetRacks().GetTargets()
		require.Len(t, targets, 1)
		assert.Equal(t, rackID, targets[0].GetExternalId())
		assert.Nil(t, targets[0].GetId())
	})
}

func TestAPITrayValidateAllRequest_SlotFilter(t *testing.T) {
	rackName := "Rack-001"
	siteID := uuid.New().String()
	int32Ptr := func(v int32) *int32 { return &v }

	t.Run("nil request has no slot filter and matches everything", func(t *testing.T) {
		var r *APITrayValidateAllRequest
		assert.False(t, r.HasSlotFilter())
		assert.True(t, r.MatchesSlot(&flowv1.Component{}))
	})

	t.Run("request without slot has no slot filter", func(t *testing.T) {
		r := &APITrayValidateAllRequest{SiteID: siteID, RackName: &rackName}
		assert.False(t, r.HasSlotFilter())
		assert.True(t, r.MatchesSlot(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 99},
		}))
	})

	t.Run("slotId matches position.slotId", func(t *testing.T) {
		r := &APITrayValidateAllRequest{SiteID: siteID, RackName: &rackName, SlotID: int32Ptr(3)}
		assert.True(t, r.HasSlotFilter())
		assert.True(t, r.MatchesSlot(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 3, TrayIdx: 7},
		}))
		assert.False(t, r.MatchesSlot(&flowv1.Component{
			Position: &flowv1.RackPosition{SlotId: 4},
		}))
	})

	t.Run("QueryValues includes slotId when set", func(t *testing.T) {
		r := &APITrayValidateAllRequest{SiteID: siteID, RackName: &rackName, SlotID: int32Ptr(3)}
		v := r.QueryValues()
		assert.Equal(t, siteID, v.Get("siteId"))
		assert.Equal(t, rackName, v.Get("rackName"))
		assert.Equal(t, "3", v.Get("slotId"))
	})
}

func TestGetProtoTrayOrderByFromQueryParam(t *testing.T) {
	tests := []struct {
		field     string
		direction string
		wantNil   bool
	}{
		{"name", "ASC", false},
		{"manufacturer", "DESC", false},
		{"model", "ASC", false},
		{"type", "DESC", false},
		{"invalid", "ASC", true},
		{"name", "asc", false},
	}
	for _, tt := range tests {
		t.Run(tt.field+"_"+tt.direction, func(t *testing.T) {
			got := GetProtoTrayOrderByFromQueryParam(tt.field, tt.direction)
			if tt.wantNil {
				assert.Nil(t, got)
				return
			}
			assert.NotNil(t, got)
			assert.Equal(t, tt.direction, got.Direction)
			assert.NotNil(t, got.GetComponentField())
		})
	}
}
