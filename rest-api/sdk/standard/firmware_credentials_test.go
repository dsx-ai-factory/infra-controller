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
	single.SetFirmwareDownloadAuth(download)
	single.SetDeviceCredentialOverrides(credentials)
	batch := NewBatchTrayFirmwareUpdateRequest("site")
	batch.SetFirmwareDownloadAuth(download)
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
			require.NoError(t, json.Unmarshal(got["firmwareDownloadAuth"], &auth))
			require.Equal(t, "download-value", auth.GetShared())
			var logins DeviceCredentialOverrides
			require.NoError(t, json.Unmarshal(got["deviceCredentialOverrides"], &logins))
			require.Equal(t, "device-value", logins.GetBmc().Password)
		})
	}
}
