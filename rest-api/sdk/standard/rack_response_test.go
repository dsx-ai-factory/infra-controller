// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestRack_UnmarshalJSON(t *testing.T) {
	const payload = `{"id":"rack-1","name":"Rack 1","manufacturer":"NVIDIA","model":"GB200","serialNumber":"SN-1","description":"","operationStatus":"Unknown","operationStatusDetails":null,"health":null,"nvLinkDomainIds":[],"taskStats":{"pendingTaskCount":0,"activeTaskCount":0}}`
	tests := []struct {
		name    string
		missing string
		wantErr bool
	}{
		{name: "accepts explicit nulls"},
		{name: "accepts omitted nullable details", missing: "operationStatusDetails"},
		{name: "accepts omitted nullable health", missing: "health"},
		{name: "rejects missing id", missing: "id", wantErr: true},
		{name: "rejects missing operationStatus", missing: "operationStatus", wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var fields map[string]any
			require.NoError(t, json.Unmarshal([]byte(payload), &fields))
			if tt.missing != "" {
				delete(fields, tt.missing)
			}
			encoded, err := json.Marshal([]map[string]any{fields})
			require.NoError(t, err)
			var racks []Rack
			err = json.Unmarshal(encoded, &racks)
			if tt.wantErr {
				require.ErrorContains(t, err, "required property "+tt.missing)
				return
			}
			require.NoError(t, err)
			require.Len(t, racks, 1)
			if tt.missing != "operationStatusDetails" {
				require.True(t, racks[0].OperationStatusDetails.IsSet())
				require.Nil(t, racks[0].OperationStatusDetails.Get())
			}
			if tt.missing != "health" {
				require.True(t, racks[0].Health.IsSet())
				require.Nil(t, racks[0].Health.Get())
			}
		})
	}
}
