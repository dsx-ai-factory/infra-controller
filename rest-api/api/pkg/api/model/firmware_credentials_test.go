// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestFirmwareRequestCredentialFields(t *testing.T) {
	cases := []struct {
		name, body string
		valid      bool
	}{
		{"new download name", `{"siteId":"site","firmwareDownloadAuth":{"shared":"download-value"}}`, true},
		{"legacy download name", `{"siteId":"site","authenticationData":{"shared":"download-value"}}`, true},
		{"aliases conflict", `{"siteId":"site","firmwareDownloadAuth":{"shared":"a"},"authenticationData":{"shared":"b"}}`, false},
		{"null alias is absent", `{"siteId":"site","authenticationData":null,"firmwareDownloadAuth":{"shared":"download-value"}}`, true},
		{"device pair", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{"bmc":{"username":"operator","password":"device-value"}}}`, true},
		{"missing direct routing", `{"siteId":"site","deviceCredentialOverrides":{"bmc":{"username":"operator","password":"device-value"}}}`, false},
		{"empty overrides", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{}}`, false},
		{"incomplete pair", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{"bmc":{"username":"operator"}}}`, false},
		{"unknown credential field", `{"siteId":"site","overrideReadinessCheck":true,"deviceCredentialOverrides":{"token":"secret"}}`, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			for _, makeRequest := range []func() interface{ Validate() error }{
				func() interface{ Validate() error } { return &APIUpdateFirmwareRequest{} },
				func() interface{ Validate() error } { return &APIBatchTrayFirmwareUpdateRequest{} },
				func() interface{ Validate() error } { return &APIBatchRackFirmwareUpdateRequest{} },
			} {
				req := makeRequest()
				err := json.Unmarshal([]byte(tc.body), req)
				if err == nil {
					err = req.Validate()
				}
				if tc.valid {
					require.NoError(t, err)
					if downloader, ok := req.(interface {
						DownloadAuth() *flowv1.FirmwareAuthenticationData
					}); ok && (tc.name == "new download name" || tc.name == "legacy download name" || tc.name == "null alias is absent") {
						require.Equal(t, "download-value", downloader.DownloadAuth().GetShared())
					}
				} else {
					require.Error(t, err)
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
