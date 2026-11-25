// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestFirmwareCredentialRequestJSON(t *testing.T) {
	download := FirmwareAuthenticationData{}
	download.SetShared("download-value")
	credentials := DeviceCredentialOverrides{}
	credentials.SetBmc(*NewDeviceLogin("operator", "device-value"))
	single := NewFirmwareUpdateRequest("site")
	single.SetDownloadCredentials(download)
	single.SetDeviceCredentialOverrides(credentials)
	batch := NewBatchTrayFirmwareUpdateRequest("site")
	batch.SetDownloadCredentials(download)
	batch.SetDeviceCredentialOverrides(credentials)
	for _, tc := range []struct {
		name    string
		request any
	}{
		{"single tray", single}, {"batch trays", batch},
	} {
		t.Run(tc.name, func(t *testing.T) {
			encoded, err := json.Marshal(tc.request)
			require.NoError(t, err)
			var got map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(encoded, &got))
			require.NotContains(t, got, "authenticationData")
			var auth FirmwareAuthenticationData
			require.NoError(t, json.Unmarshal(got["downloadCredentials"], &auth))
			require.Equal(t, "download-value", auth.GetShared())
			var logins DeviceCredentialOverrides
			require.NoError(t, json.Unmarshal(got["deviceCredentialOverrides"], &logins))
			require.Equal(t, "device-value", logins.GetBmc().Password)
		})
	}
}

func TestFirmwareUpdateRequest_UnmarshalJSON(t *testing.T) {
	for _, tc := range []struct {
		name              string
		request           any
		deviceCredentials bool
	}{
		{"single tray or rack", &FirmwareUpdateRequest{}, true},
		{"batch trays", &BatchTrayFirmwareUpdateRequest{}, true},
		{"batch racks", &BatchRackFirmwareUpdateRequest{}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			for _, explicitNull := range []bool{false, true} {
				t.Run(map[bool]string{false: "omitted", true: "null"}[explicitNull], func(t *testing.T) {
					values := map[string]any{"siteId": "58fe7a29-58b7-4d61-9b64-e355eaff6024"}
					if explicitNull {
						values["authenticationData"] = nil
						values["downloadCredentials"] = nil
						if tc.deviceCredentials {
							values["deviceCredentialOverrides"] = nil
						}
					}
					payload, err := json.Marshal(values)
					require.NoError(t, err)
					require.NoError(t, json.Unmarshal(payload, tc.request))
					encoded, err := json.Marshal(tc.request)
					require.NoError(t, err)
					require.JSONEq(t, string(payload), string(encoded))
				})
			}
		})
	}
}
