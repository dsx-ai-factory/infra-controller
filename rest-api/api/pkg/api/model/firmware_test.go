// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestAPIFirmwareAuthenticationData_UnmarshalJSON(t *testing.T) {
	tests := []struct {
		name         string
		payload      string
		wantErr      string
		wantNVSwitch *string
	}{
		{
			name:         "supported per-component field",
			payload:      `{"perComponent":{"nvswitch":"token"}}`,
			wantNVSwitch: strPtr("token"),
		},
		{
			name:    "unknown authentication representation",
			payload: `{"perTray":{"nvswitch":"token"}}`,
			wantErr: `authenticationData contains unknown field "perTray"`,
		},
		{
			name:    "unknown per-component field",
			payload: `{"perComponent":{"switch":"token"}}`,
			wantErr: `authenticationData.perComponent contains unknown field "switch"`,
		},
		{
			name:    "supported and unknown per-component fields",
			payload: `{"perComponent":{"nvswitch":"valid-token","switch":"other-token"}}`,
			wantErr: `authenticationData.perComponent contains unknown field "switch"`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var got APIFirmwareAuthenticationData
			err := json.Unmarshal([]byte(tt.payload), &got)
			if tt.wantErr != "" {
				assert.EqualError(t, err, tt.wantErr)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, tt.wantNVSwitch, got.PerComponent.NVSwitch)
		})
	}
}

func TestAPIUpdateFirmwareRequest_Validate(t *testing.T) {
	t.Run("credential fields", func(t *testing.T) {
		testFirmwareRequestCredentialFields(t, func() interface{ Validate() error } { return &APIUpdateFirmwareRequest{} })
	})
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
	t.Run("credential fields", func(t *testing.T) {
		testFirmwareRequestCredentialFields(t, func() interface{ Validate() error } { return &APIBatchTrayFirmwareUpdateRequest{} })
	})
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
	t.Run("credential fields", func(t *testing.T) { testRackFirmwareRequestCredentialFields(t, &APIBatchRackFirmwareUpdateRequest{}) })
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

func testFirmwareRequestCredentialFields(t *testing.T, newRequest func() interface{ Validate() error }) {
	t.Helper()
	cases := []struct {
		name, body string
		valid      bool
	}{
		{"new download name", `{"siteId":"site","downloadCredentials":{"shared":"download-value"}}`, true},
		{"legacy download name", `{"siteId":"site","authenticationData":{"shared":"download-value"}}`, true},
		{"aliases conflict", `{"siteId":"site","downloadCredentials":{"shared":"a"},"authenticationData":{"shared":"b"}}`, false},
		{"null alias is absent", `{"siteId":"site","authenticationData":null,"downloadCredentials":{"shared":"download-value"}}`, true},
		{"null credentials are absent", `{"siteId":"site","authenticationData":null,"downloadCredentials":null,"deviceCredentialOverrides":null}`, true},
		{"device pair", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{"bmc":{"username":"operator","password":"device-value"}}}`, true},
		{"missing direct routing", `{"siteId":"site","deviceCredentialOverrides":{"bmc":{"username":"operator","password":"device-value"}}}`, false},
		{"empty overrides", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{}}`, false},
		{"incomplete pair", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{"bmc":{"username":"operator"}}}`, false},
		{"unknown credential field", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{"token":"secret"}}`, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := newRequest()
			err := json.Unmarshal([]byte(tc.body), req)
			if err == nil {
				err = req.Validate()
			}
			if tc.valid {
				require.NoError(t, err)
				downloader, ok := req.(interface {
					DownloadCredentialsToProto() *flowv1.FirmwareAuthenticationData
				})
				if ok && (tc.name == "new download name" || tc.name == "legacy download name" || tc.name == "null alias is absent") {
					require.Equal(t, "download-value", downloader.DownloadCredentialsToProto().GetShared())
				}
			} else {
				require.Error(t, err)
				if tc.name == "missing direct routing" {
					var fields validation.Errors
					require.ErrorAs(t, err, &fields)
					require.Contains(t, fields, "deviceCredentialOverrides")
					require.NotContains(t, fields, "authenticationData")
					require.EqualError(t, fields["deviceCredentialOverrides"], "requires overrideReadinessCheck")
				}
			}
		})
	}
}

func TestAPIDeviceCredentialOverrides_ToProto(t *testing.T) {
	cases := []struct {
		name  string
		value *APIDeviceCredentialOverrides
	}{
		{"absent", nil},
		{"bmc", &APIDeviceCredentialOverrides{BMC: &APIDeviceLogin{Username: "bmc-user", Password: "bmc-value"}}},
		{"nvos", &APIDeviceCredentialOverrides{NVOS: &APIDeviceLogin{Username: "nvos-user", Password: "nvos-value"}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := tc.value.ToProto()
			if tc.value == nil {
				require.Nil(t, got)
				return
			}
			if tc.value.BMC != nil {
				require.Equal(t, "bmc-user", got.GetBmc().GetUsername())
				require.Equal(t, "bmc-value", got.GetBmc().GetPassword())
				require.Nil(t, got.Nvos)
			}
			if tc.value.NVOS != nil {
				require.Equal(t, "nvos-user", got.GetNvos().GetUsername())
				require.Equal(t, "nvos-value", got.GetNvos().GetPassword())
				require.Nil(t, got.Bmc)
			}
		})
	}
}

func testRackFirmwareRequestCredentialFields(t *testing.T, req interface{ Validate() error }) {
	t.Helper()
	for _, value := range []string{`{"bmc":{"username":"operator","password":"device-value"}}`, `{"unknown":"ignored"}`, `42`} {
		t.Run(value, func(t *testing.T) {
			body := `{"siteId":"site","deviceCredentialOverrides":` + value + `}`
			require.NoError(t, json.Unmarshal([]byte(body), req))
			require.NoError(t, req.Validate())
		})
	}
}

func TestAPIRackFirmwareUpdateRequest_Validate(t *testing.T) {
	testRackFirmwareRequestCredentialFields(t, &APIRackFirmwareUpdateRequest{})
}

func TestAPIUpdateFirmwareRequest_UnmarshalJSON(t *testing.T) {
	testFirmwareRequestAliasErrors(t, func() interface{} { return &APIUpdateFirmwareRequest{} })
}

func TestAPIRackFirmwareUpdateRequest_UnmarshalJSON(t *testing.T) {
	testFirmwareRequestAliasErrors(t, func() interface{} { return &APIRackFirmwareUpdateRequest{} })
}

func TestAPIBatchRackFirmwareUpdateRequest_UnmarshalJSON(t *testing.T) {
	testFirmwareRequestAliasErrors(t, func() interface{} { return &APIBatchRackFirmwareUpdateRequest{} })
}

func TestAPIBatchTrayFirmwareUpdateRequest_UnmarshalJSON(t *testing.T) {
	testFirmwareRequestAliasErrors(t, func() interface{} { return &APIBatchTrayFirmwareUpdateRequest{} })
}

func testFirmwareRequestAliasErrors(t *testing.T, newRequest func() interface{}) {
	t.Helper()
	for _, tc := range []struct{ name, body, want string }{
		{"preferred", `{"downloadCredentials":{"bogus":"not-a-secret"}}`, `downloadCredentials contains unknown field "bogus"`},
		{"legacy", `{"authenticationData":{"bogus":"not-a-secret"}}`, `authenticationData contains unknown field "bogus"`},
		{"preferred nested", `{"downloadCredentials":{"perComponent":{"bogus":"not-a-secret"}}}`, `downloadCredentials.perComponent contains unknown field "bogus"`},
		{"legacy nested", `{"authenticationData":{"perComponent":{"bogus":"not-a-secret"}}}`, `authenticationData.perComponent contains unknown field "bogus"`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			require.EqualError(t, json.Unmarshal([]byte(tc.body), newRequest()), tc.want)
		})
	}
}
