// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

// The REST server sends `"driveModel": null` for every component that is not a
// Drive, while OpenAPI models nullable response fields as optional (`driveModel`
// is nullable and non-required).
func TestHostFirmwareComponent_UnmarshalJSON(t *testing.T) {
	tests := []struct {
		name              string
		payload           string
		wantErr           string
		wantDriveModelSet bool
		wantDriveModel    *string
	}{
		{
			name:              "non-drive component with null driveModel",
			payload:           `{"type":"BMC","driveModel":null,"firmware":[]}`,
			wantDriveModelSet: true,
		},
		{
			name:              "drive component",
			payload:           `{"type":"Drive","driveModel":"ST24000NM005H","firmware":[]}`,
			wantDriveModelSet: true,
			wantDriveModel:    PtrString("ST24000NM005H"),
		},
		{
			name:    "omitted driveModel",
			payload: `{"type":"BMC","firmware":[]}`,
		},
		{
			name:    "missing required type",
			payload: `{"firmware":[]}`,
			wantErr: "required property type",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var component HostFirmwareComponent
			err := json.Unmarshal([]byte(tt.payload), &component)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tt.wantDriveModelSet, component.DriveModel.IsSet())
			require.Equal(t, tt.wantDriveModel, component.DriveModel.Get())

			encoded, err := json.Marshal(component)
			require.NoError(t, err)
			var fields map[string]any
			require.NoError(t, json.Unmarshal(encoded, &fields))
			if tt.wantDriveModelSet {
				require.Contains(t, fields, "driveModel")
			} else {
				require.NotContains(t, fields, "driveModel")
			}
		})
	}
}

// The REST server writes an ordering entry without a drive model as its bare
// component type and a Drive as an object, so the SDK must read both.
func TestHostFirmwareOrderingEntry_UnmarshalJSON(t *testing.T) {
	var ordering []HostFirmwareOrderingEntry
	require.NoError(t, json.Unmarshal([]byte(`["BMC",{"type":"Drive","driveModel":"ST24000NM005H"}]`), &ordering))
	require.Len(t, ordering, 2)
	require.Equal(t, HOSTFIRMWARECOMPONENTTYPE_BMC.Ptr(), ordering[0].HostFirmwareComponentType)
	require.Nil(t, ordering[0].HostFirmwareComponentRef)
	require.Nil(t, ordering[1].HostFirmwareComponentType)
	require.Equal(t, PtrString("ST24000NM005H"), ordering[1].HostFirmwareComponentRef.DriveModel.Get())
}
