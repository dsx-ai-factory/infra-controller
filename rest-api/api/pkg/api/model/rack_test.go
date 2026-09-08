// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
)

func TestAPIRackJSONContract(t *testing.T) {
	description := "Core rack description"
	modelName := "NICO-QA-RACK"
	apiRack := NewAPIRack(&flowv1.Rack{
		ExternalId: "rack-01",
		Info: &flowv1.DeviceInfo{
			Model:       &modelName,
			Description: &description,
		},
		Location: &flowv1.Location{Datacenter: "DC1"},
	}, false)

	got, err := json.Marshal(apiRack)
	assert.NoError(t, err)
	assert.JSONEq(t, `{
		"id":"rack-01",
		"name":"",
		"manufacturer":"",
		"model":"NICO-QA-RACK",
		"serialNumber":"",
		"description":"Core rack description",
		"nvLinkDomainIds":[],
		"location":{"region":"","datacenter":"DC1","room":"","position":""},
		"taskStats":{"pendingTaskCount":0,"activeTaskCount":0}
	}`, string(got))
}

func TestAPIComponentDiffJSONContract(t *testing.T) {
	got, err := json.Marshal(APIComponentDiff{})
	assert.NoError(t, err)
	assert.JSONEq(t, `{"type":"","id":null,"macAddress":null}`, string(got))
}

func TestAPIComponentDiff_FromProto(t *testing.T) {
	tests := []struct {
		name    string
		input   *flowv1.ComponentDiff
		wantID  *string
		wantMAC *string
	}{
		{
			name:   "component ID",
			input:  &flowv1.ComponentDiff{ComponentId: "machine-01"},
			wantID: cutil.GetPtr("machine-01"),
		},
		{
			name:    "missing component MAC",
			input:   &flowv1.ComponentDiff{ComponentMacAddress: "aa:bb:cc:dd:ee:ff"},
			wantMAC: cutil.GetPtr("aa:bb:cc:dd:ee:ff"),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var got APIComponentDiff
			got.FromProto(tt.input)
			assert.Equal(t, tt.wantID, got.ID)
			assert.Equal(t, tt.wantMAC, got.MACAddress)
		})
	}
}

func TestNewAPIRack(t *testing.T) {
	description := "Test rack description"
	model := "NVL72"
	domainID := "59202b81-65fb-45ec-b3b8-91ab0ad3f34a"
	domainID2 := "cfa95885-186f-49b7-993f-dccd417a67cb"

	tests := []struct {
		name           string
		rack           *flowv1.Rack
		withComponents bool
		want           *APIRack
	}{
		{
			name:           "nil rack returns nil",
			rack:           nil,
			withComponents: false,
			want:           nil,
		},
		{
			name: "basic rack without components",
			rack: &flowv1.Rack{
				ExternalId: "core-rack-1",
				Info: &flowv1.DeviceInfo{
					Id:           &flowv1.UUID{Id: "flow-rack-uuid"},
					Name:         "test-rack",
					Manufacturer: "NVIDIA",
					Model:        &model,
					SerialNumber: "SN12345",
					Description:  &description,
				},
				Location: &flowv1.Location{
					Region:     "us-west-2",
					Datacenter: "DC1",
					Room:       "Room-A",
					Position:   "Row-1-Pos-5",
				},
			},
			withComponents: false,
			want: &APIRack{
				ID:           "core-rack-1",
				Name:         "test-rack",
				Manufacturer: "NVIDIA",
				Model:        "NVL72",
				SerialNumber: "SN12345",
				Description:  "Test rack description",
				Location: &APIRackLocation{
					Region:     "us-west-2",
					Datacenter: "DC1",
					Room:       "Room-A",
					Position:   "Row-1-Pos-5",
				},
				Components: nil,
			},
		},
		{
			name: "rack with NVLink domain memberships",
			rack: &flowv1.Rack{
				ExternalId: "core-rack-in-domain",
				Info:       &flowv1.DeviceInfo{Id: &flowv1.UUID{Id: "flow-rack-uuid"}},
				NvlDomainIds: []*flowv1.UUID{
					{Id: domainID},
					{Id: domainID2},
				},
			},
			want: &APIRack{
				ID:              "core-rack-in-domain",
				NVLinkDomainIDs: []string{domainID, domainID2},
			},
		},
		{
			name: "rack with components",
			rack: &flowv1.Rack{
				ExternalId: "core-rack-with-components",
				Info: &flowv1.DeviceInfo{
					Id:   &flowv1.UUID{Id: "flow-rack-uuid"},
					Name: "rack-1",
				},
				Components: []*flowv1.Component{
					{
						Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
						Info: &flowv1.DeviceInfo{
							Id:           &flowv1.UUID{Id: "comp-1"},
							Name:         "compute-node-1",
							SerialNumber: "CSN001",
							Manufacturer: "NVIDIA",
						},
						FirmwareVersion: "1.0.0",
						Position: &flowv1.RackPosition{
							SlotId: 1,
						},
						ComponentId:    "nico-machine-123",
						RackExternalId: "core-rack-with-components",
						Status:         &flowv1.ComponentOperationStatus{Phase: flowv1.Phase_PHASE_READY},
						LeakStatus:     flowv1.LeakStatus_LEAK_STATUS_NOT_DETECTED,
					},
					{
						Type:           flowv1.ComponentType_COMPONENT_TYPE_TORSWITCH,
						ComponentId:    "nico-switch-456",
						RackExternalId: "core-rack-with-components",
						Info: &flowv1.DeviceInfo{
							Id:   &flowv1.UUID{Id: "comp-2"},
							Name: "switch-1",
						},
						Position: &flowv1.RackPosition{
							SlotId: 48,
						},
					},
				},
			},
			withComponents: true,
			want: &APIRack{
				ID:   "core-rack-with-components",
				Name: "rack-1",
				Components: []*APIRackComponent{
					{
						ID:              "nico-machine-123",
						RackID:          "core-rack-with-components",
						Type:            "Compute",
						Name:            "compute-node-1",
						SerialNumber:    "CSN001",
						Manufacturer:    "NVIDIA",
						FirmwareVersion: "1.0.0",
						SlotID:          1,
						OperationStatus: "Ready",
						LeakStatus:      "NoLeak",
					},
					{
						ID:              "nico-switch-456",
						RackID:          "core-rack-with-components",
						Type:            "TORSwitch",
						Name:            "switch-1",
						SlotID:          48,
						OperationStatus: "Unknown",
						LeakStatus:      "Unknown",
					},
				},
			},
		},
		{
			name: "rack with components but withComponents=false",
			rack: &flowv1.Rack{
				ExternalId: "core-rack-id",
				Info: &flowv1.DeviceInfo{
					Id:   &flowv1.UUID{Id: "flow-rack-uuid"},
					Name: "rack-name",
				},
				Components: []*flowv1.Component{
					{
						Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
						Info: &flowv1.DeviceInfo{
							Id:   &flowv1.UUID{Id: "comp-1"},
							Name: "compute-node-1",
						},
					},
				},
			},
			withComponents: false,
			want: &APIRack{
				ID:         "core-rack-id",
				Name:       "rack-name",
				Components: nil,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIRack(tt.rack, tt.withComponents)

			if tt.want == nil {
				assert.Nil(t, got)
				return
			}

			assert.NotNil(t, got)
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, tt.want.Manufacturer, got.Manufacturer)
			assert.Equal(t, tt.want.Model, got.Model)
			assert.Equal(t, tt.want.SerialNumber, got.SerialNumber)
			assert.Equal(t, tt.want.Description, got.Description)
			assert.ElementsMatch(t, tt.want.NVLinkDomainIDs, got.NVLinkDomainIDs)

			if tt.want.Location != nil {
				assert.NotNil(t, got.Location)
				assert.Equal(t, tt.want.Location.Region, got.Location.Region)
				assert.Equal(t, tt.want.Location.Datacenter, got.Location.Datacenter)
				assert.Equal(t, tt.want.Location.Room, got.Location.Room)
				assert.Equal(t, tt.want.Location.Position, got.Location.Position)
			}

			if tt.want.Components != nil {
				assert.NotNil(t, got.Components)
				assert.Equal(t, len(tt.want.Components), len(got.Components))
				for i, wantComp := range tt.want.Components {
					gotComp := got.Components[i]
					assert.Equal(t, wantComp.ID, gotComp.ID)
					assert.Equal(t, wantComp.Type, gotComp.Type)
					assert.Equal(t, wantComp.Name, gotComp.Name)
					assert.Equal(t, wantComp.SerialNumber, gotComp.SerialNumber)
					assert.Equal(t, wantComp.Manufacturer, gotComp.Manufacturer)
					assert.Equal(t, wantComp.FirmwareVersion, gotComp.FirmwareVersion)
					assert.Equal(t, wantComp.SlotID, gotComp.SlotID)
					assert.Equal(t, wantComp.OperationStatus, gotComp.OperationStatus)
					assert.Equal(t, wantComp.LeakStatus, gotComp.LeakStatus)
				}
			} else {
				assert.Nil(t, got.Components)
			}
		})
	}
}

func TestAPIBringUpRackRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIBringUpRackRequest
		wantErr bool
	}{
		{
			name:    "valid - with siteId",
			request: APIBringUpRackRequest{SiteID: "site-1"},
			wantErr: false,
		},
		{
			name:    "valid - with siteId and description",
			request: APIBringUpRackRequest{SiteID: "site-1", Description: "bring up rack"},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIBringUpRackRequest{},
			wantErr: true,
		},
		{
			name:    "invalid - empty siteId",
			request: APIBringUpRackRequest{SiteID: ""},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestNewAPIBringUpRackResponse(t *testing.T) {
	tests := []struct {
		name     string
		resp     *flowv1.SubmitTaskResponse
		expected *APIBringUpRackResponse
	}{
		{
			name:     "nil response returns empty task IDs",
			resp:     nil,
			expected: &APIBringUpRackResponse{TaskIDs: []string{}},
		},
		{
			name: "single task ID",
			resp: &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{{Id: "task-1"}},
			},
			expected: &APIBringUpRackResponse{TaskIDs: []string{"task-1"}},
		},
		{
			name: "multiple task IDs",
			resp: &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{{Id: "task-1"}, {Id: "task-2"}},
			},
			expected: &APIBringUpRackResponse{TaskIDs: []string{"task-1", "task-2"}},
		},
		{
			name:     "empty task IDs",
			resp:     &flowv1.SubmitTaskResponse{},
			expected: &APIBringUpRackResponse{TaskIDs: []string{}},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := NewAPIBringUpRackResponse(tt.resp)
			assert.NotNil(t, result)
			assert.Equal(t, tt.expected.TaskIDs, result.TaskIDs)
		})
	}
}

func TestAPIBatchBringUpRackRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIBatchBringUpRackRequest
		wantErr bool
	}{
		{
			name:    "valid - with siteId only",
			request: APIBatchBringUpRackRequest{SiteID: "site-1"},
			wantErr: false,
		},
		{
			name: "valid - with filter",
			request: APIBatchBringUpRackRequest{
				SiteID: "site-1",
				Filter: &RackFilter{Names: []string{"Rack-001"}},
			},
			wantErr: false,
		},
		{
			name: "valid - with description",
			request: APIBatchBringUpRackRequest{
				SiteID:      "site-1",
				Description: "batch bring up",
			},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIBatchBringUpRackRequest{},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
