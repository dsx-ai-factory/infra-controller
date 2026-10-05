// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	flowv1 "github.com/dsx-ai-factory/infra-controller/rest-api/proto/flow/gen/v1"
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
	states := []struct {
		canonical string
		legacy    string
	}{
		{canonical: PowerControlStateOn, legacy: "on"},
		{canonical: PowerControlStateOff, legacy: "off"},
		{canonical: PowerControlStateCycle, legacy: "cycle"},
		{canonical: PowerControlStateForceOff, legacy: "forceoff"},
		{canonical: PowerControlStateForceCycle, legacy: "forcecycle"},
		{canonical: PowerControlStateACCycle, legacy: "acpowercycle"},
	}
	for _, state := range states {
		t.Run("accepts canonical "+state.canonical, func(t *testing.T) {
			request := APIUpdatePowerStateRequest{SiteID: "site-1", State: state.canonical}
			assert.NoError(t, request.Validate())
			assert.Equal(t, state.canonical, request.State)
		})
		t.Run("normalizes legacy "+state.legacy, func(t *testing.T) {
			request := APIUpdatePowerStateRequest{SiteID: "site-1", State: state.legacy}
			assert.NoError(t, request.Validate())
			assert.Equal(t, state.canonical, request.State)
		})
	}

	tests := []struct {
		name    string
		request APIUpdatePowerStateRequest
		wantErr bool
	}{
		{
			name:    "invalid - missing siteId",
			request: APIUpdatePowerStateRequest{State: PowerControlStateOn},
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
		{
			name:    "invalid - arbitrary mixed case",
			request: APIUpdatePowerStateRequest{SiteID: "site-1", State: "Forcecycle"},
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

func TestPowerControlStateWorkflowToken(t *testing.T) {
	tests := []struct {
		state string
		want  string
	}{
		{state: PowerControlStateOn, want: "on"},
		{state: PowerControlStateOff, want: "off"},
		{state: PowerControlStateCycle, want: "cycle"},
		{state: PowerControlStateForceOff, want: "forceoff"},
		{state: PowerControlStateForceCycle, want: "forcecycle"},
		{state: PowerControlStateACCycle, want: "acpowercycle"},
		{state: "acpowercycle", want: "acpowercycle"},
		{state: "unknown", want: "unknown"},
	}

	for _, test := range tests {
		t.Run(test.state, func(t *testing.T) {
			assert.Equal(t, test.want, PowerControlStateWorkflowToken(test.state))
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
		name      string
		request   APIBatchUpdateRackPowerStateRequest
		wantState string
		wantErr   bool
	}{
		{
			name:      "normalizes legacy on with siteId",
			request:   APIBatchUpdateRackPowerStateRequest{SiteID: "site-1", State: "on"},
			wantState: PowerControlStateOn,
		},
		{
			name: "normalizes legacy off with filter",
			request: APIBatchUpdateRackPowerStateRequest{
				SiteID: "site-1",
				Filter: &RackFilter{Names: []string{"Rack-001"}},
				State:  "off",
			},
			wantState: PowerControlStateOff,
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
				assert.Equal(t, tt.wantState, tt.request.State)
			}
		})
	}
}

func TestAPIBatchUpdateTrayPowerStateRequest_Validate(t *testing.T) {
	tests := []struct {
		name      string
		request   APIBatchUpdateTrayPowerStateRequest
		wantState string
		wantErr   bool
	}{
		{
			name:      "accepts canonical state",
			request:   APIBatchUpdateTrayPowerStateRequest{SiteID: "site-1", State: PowerControlStateForceOff},
			wantState: PowerControlStateForceOff,
		},
		{
			name:      "normalizes legacy state",
			request:   APIBatchUpdateTrayPowerStateRequest{SiteID: "site-1", State: "forcecycle"},
			wantState: PowerControlStateForceCycle,
		},
		{
			name:    "rejects arbitrary mixed case",
			request: APIBatchUpdateTrayPowerStateRequest{SiteID: "site-1", State: "Forcecycle"},
			wantErr: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, test.wantState, test.request.State)
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
