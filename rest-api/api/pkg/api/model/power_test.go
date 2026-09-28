// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
)

func TestAPIUpdatePowerStateRequest_OverrideReadinessCheck(t *testing.T) {
	var omitted APIUpdatePowerStateRequest
	assert.NoError(t, json.Unmarshal([]byte(`{"siteId":"s","state":"on"}`), &omitted))
	assert.False(t, omitted.OverrideReadinessCheck, "defaults to false when omitted")

	var optIn APIUpdatePowerStateRequest
	assert.NoError(t, json.Unmarshal([]byte(`{"siteId":"s","state":"on","overrideReadinessCheck":true}`), &optIn))
	assert.True(t, optIn.OverrideReadinessCheck, "set when provided")
}

func TestAPIUpdatePowerStateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIUpdatePowerStateRequest
		wantErr bool
	}{
		{
			name:    "valid - on",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "on"},
			wantErr: false,
		},
		{
			name:    "valid - off",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "off"},
			wantErr: false,
		},
		{
			name:    "valid - cycle",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "cycle"},
			wantErr: false,
		},
		{
			name:    "valid - forceoff",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "forceoff"},
			wantErr: false,
		},
		{
			name:    "valid - forcecycle",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "forcecycle"},
			wantErr: false,
		},
		{
			name:    "valid - acpowercycle",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "acpowercycle"},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIUpdatePowerStateRequest{State: "on"},
			wantErr: true,
		},
		{
			name:    "invalid - empty state",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: ""},
			wantErr: true,
		},
		{
			name:    "invalid - unknown state",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "reboot"},
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

func TestNewAPIUpdatePowerStateResponse(t *testing.T) {
	tests := []struct {
		name     string
		resp     *flowv1.SubmitTaskResponse
		expected *APIUpdatePowerStateResponse
	}{
		{
			name:     "nil response returns empty task IDs",
			resp:     nil,
			expected: &APIUpdatePowerStateResponse{TaskIDs: []string{}},
		},
		{
			name: "response with task IDs",
			resp: &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{
					{Id: "task-1"},
					{Id: "task-2"},
				},
			},
			expected: &APIUpdatePowerStateResponse{TaskIDs: []string{"task-1", "task-2"}},
		},
		{
			name: "response with empty task IDs",
			resp: &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{},
			},
			expected: &APIUpdatePowerStateResponse{TaskIDs: []string{}},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIUpdatePowerStateResponse(tt.resp)
			assert.NotNil(t, got)
			assert.Equal(t, tt.expected.TaskIDs, got.TaskIDs)
		})
	}
}

func TestAPIBatchUpdateRackPowerStateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIBatchUpdateRackPowerStateRequest
		wantErr bool
	}{
		{
			name:    "valid - on with siteId",
			request: APIBatchUpdateRackPowerStateRequest{SiteID: "site-1", State: "on"},
			wantErr: false,
		},
		{
			name: "valid - with filter",
			request: APIBatchUpdateRackPowerStateRequest{
				SiteID: "site-1",
				Filter: &RackFilter{Names: []string{"Rack-001"}},
				State:  "off",
			},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIBatchUpdateRackPowerStateRequest{State: "on"},
			wantErr: true,
		},
		{
			name:    "invalid - bad state",
			request: APIBatchUpdateRackPowerStateRequest{SiteID: "site-1", State: "reboot"},
			wantErr: true,
		},
		{
			name:    "invalid - empty state",
			request: APIBatchUpdateRackPowerStateRequest{SiteID: "site-1"},
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

func TestRackFilter_ToTargetSpec(t *testing.T) {
	tests := []struct {
		name          string
		filter        *RackFilter
		expectedRacks int
	}{
		{
			name:          "nil filter - targets all racks",
			filter:        nil,
			expectedRacks: 1,
		},
		{
			name:          "empty filter - targets all racks",
			filter:        &RackFilter{},
			expectedRacks: 1,
		},
		{
			name:          "with single name",
			filter:        &RackFilter{Names: []string{"Rack-001"}},
			expectedRacks: 1,
		},
		{
			name:          "with multiple names",
			filter:        &RackFilter{Names: []string{"Rack-001", "Rack-002"}},
			expectedRacks: 2,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			spec := tt.filter.ToTargetSpec()
			assert.NotNil(t, spec)

			racks := spec.GetRacks()
			assert.NotNil(t, racks)
			assert.Len(t, racks.GetTargets(), tt.expectedRacks)
		})
	}
}
