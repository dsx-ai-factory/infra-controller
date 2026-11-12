// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

// The REST server sends `"driveModel": null` for every component that is not a
// Drive, so the SDK must accept a required null there.
func TestHostFirmwareComponent_UnmarshalJSON(t *testing.T) {
	tests := []struct {
		name       string
		payload    string
		driveModel *string
	}{
		{
			name:    "non-drive component with null driveModel",
			payload: `{"type":"BMC","driveModel":null,"firmware":[]}`,
		},
		{
			name:       "drive component",
			payload:    `{"type":"Drive","driveModel":"ST24000NM005H","firmware":[]}`,
			driveModel: PtrString("ST24000NM005H"),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var component HostFirmwareComponent
			require.NoError(t, json.Unmarshal([]byte(tt.payload), &component))
			require.Equal(t, tt.driveModel, component.DriveModel.Get())

			encoded, err := json.Marshal(component)
			require.NoError(t, err)
			var fields map[string]any
			require.NoError(t, json.Unmarshal(encoded, &fields))
			require.Contains(t, fields, "driveModel")
		})
	}

	t.Run("rejects missing driveModel", func(t *testing.T) {
		var component HostFirmwareComponent
		err := json.Unmarshal([]byte(`{"type":"BMC","firmware":[]}`), &component)
		require.ErrorContains(t, err, "required property driveModel")
	})
}
