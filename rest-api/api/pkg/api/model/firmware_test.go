// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
)

func TestAPIUpdateFirmwareRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIUpdateFirmwareRequest
		wantErr bool
	}{
		{
			name:    "valid - with siteId and version",
			request: APIUpdateFirmwareRequest{SiteID: "site-1", Version: strPtr("24.11.0")},
			wantErr: false,
		},
		{
			name:    "valid - with siteId only (no version)",
			request: APIUpdateFirmwareRequest{SiteID: "site-1"},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIUpdateFirmwareRequest{Version: strPtr("24.11.0")},
			wantErr: true,
		},
		{
			name: "valid - targets with version",
			request: APIUpdateFirmwareRequest{
				SiteID:  "site-1",
				Version: strPtr("24.11.0"),
				Targets: []string{"bmc", "nvos"},
			},
			wantErr: false,
		},
		{
			name: "valid - targets without version",
			request: APIUpdateFirmwareRequest{
				SiteID:  "site-1",
				Targets: []string{"bmc"},
			},
			wantErr: false,
		},
		{
			name: "valid - targets with empty version string",
			request: APIUpdateFirmwareRequest{
				SiteID:  "site-1",
				Version: strPtr(""),
				Targets: []string{"bmc"},
			},
			wantErr: false,
		},
		{
			name: "invalid - targets contains empty string",
			request: APIUpdateFirmwareRequest{
				SiteID:  "site-1",
				Version: strPtr("24.11.0"),
				Targets: []string{"bmc", ""},
			},
			wantErr: true,
		},
		{
			name: "invalid - authentication data has no representation",
			request: APIUpdateFirmwareRequest{
				SiteID:             "site-1",
				AuthenticationData: &APIFirmwareAuthenticationData{},
			},
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

func TestAPIFirmwareAuthenticationDataValidate(t *testing.T) {
	shared := "shared-token"
	tests := []struct {
		name    string
		data    APIFirmwareAuthenticationData
		wantErr bool
	}{
		{name: "shared", data: APIFirmwareAuthenticationData{Shared: &shared}},
		{name: "per component", data: APIFirmwareAuthenticationData{PerComponent: &APIPerComponentFirmwareAuthenticationData{Compute: &shared}}},
		{name: "empty per component is accepted as no authentication", data: APIFirmwareAuthenticationData{PerComponent: &APIPerComponentFirmwareAuthenticationData{}}},
		{name: "neither", data: APIFirmwareAuthenticationData{}, wantErr: true},
		{name: "both", data: APIFirmwareAuthenticationData{Shared: &shared, PerComponent: &APIPerComponentFirmwareAuthenticationData{}}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.data.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestAPIFirmwareAuthenticationDataToProto(t *testing.T) {
	shared := "shared-token"
	compute := "compute-token"
	data := (&APIFirmwareAuthenticationData{Shared: &shared}).ToProto()
	assert.Equal(t, shared, data.GetShared())

	data = (&APIFirmwareAuthenticationData{
		PerComponent: &APIPerComponentFirmwareAuthenticationData{Compute: &compute},
	}).ToProto()
	assert.Equal(t, compute, data.GetPerComponent().GetCompute())
	assert.Nil(t, data.GetPerComponent().Nvswitch)
	assert.Nil(t, data.GetPerComponent().Powershelf)

	var absent *APIFirmwareAuthenticationData
	assert.Nil(t, absent.ToProto())
}

func TestAPIBatchTrayFirmwareUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIBatchTrayFirmwareUpdateRequest
		wantErr bool
	}{
		{
			name:    "valid - siteId only",
			request: APIBatchTrayFirmwareUpdateRequest{SiteID: "site-1"},
			wantErr: false,
		},
		{
			name: "valid - with filter and version",
			request: APIBatchTrayFirmwareUpdateRequest{
				SiteID:  "site-1",
				Filter:  &TrayFilter{IDs: []string{"550e8400-e29b-41d4-a716-446655440000"}},
				Version: strPtr("24.11.0"),
			},
			wantErr: false,
		},
		{
			name: "valid - targets with version",
			request: APIBatchTrayFirmwareUpdateRequest{
				SiteID:  "site-1",
				Version: strPtr("24.11.0"),
				Targets: []string{"bmc"},
			},
			wantErr: false,
		},
		{
			name: "valid - targets without version",
			request: APIBatchTrayFirmwareUpdateRequest{
				SiteID:  "site-1",
				Targets: []string{"bmc"},
			},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIBatchTrayFirmwareUpdateRequest{},
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

func TestNewAPIUpdateFirmwareResponse(t *testing.T) {
	tests := []struct {
		name     string
		resp     *flowv1.SubmitTaskResponse
		expected *APIUpdateFirmwareResponse
	}{
		{
			name:     "nil response returns empty task IDs",
			resp:     nil,
			expected: &APIUpdateFirmwareResponse{TaskIDs: []string{}},
		},
		{
			name: "single task ID",
			resp: &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{{Id: "task-1"}},
			},
			expected: &APIUpdateFirmwareResponse{TaskIDs: []string{"task-1"}},
		},
		{
			name: "multiple task IDs",
			resp: &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{{Id: "task-1"}, {Id: "task-2"}},
			},
			expected: &APIUpdateFirmwareResponse{TaskIDs: []string{"task-1", "task-2"}},
		},
		{
			name:     "empty task IDs",
			resp:     &flowv1.SubmitTaskResponse{},
			expected: &APIUpdateFirmwareResponse{TaskIDs: []string{}},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := NewAPIUpdateFirmwareResponse(tt.resp)
			assert.Equal(t, tt.expected, result)
		})
	}
}

func TestAPIBatchRackFirmwareUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIBatchRackFirmwareUpdateRequest
		wantErr bool
	}{
		{
			name:    "valid - with siteId only",
			request: APIBatchRackFirmwareUpdateRequest{SiteID: "site-1"},
			wantErr: false,
		},
		{
			name: "valid - with filter and version",
			request: APIBatchRackFirmwareUpdateRequest{
				SiteID:  "site-1",
				Filter:  &RackFilter{Names: []string{"rack-1"}},
				Version: strPtr("1.0"),
			},
			wantErr: false,
		},
		{
			name:    "invalid - missing siteId",
			request: APIBatchRackFirmwareUpdateRequest{},
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

func strPtr(s string) *string { return &s }
