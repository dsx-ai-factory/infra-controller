// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestRack_UnmarshalJSON(t *testing.T) {
	const payload = `{"operationStatus":"Unknown","operationStatusDetails":null,"health":null,"nvLinkDomainIds":[],"taskStats":{"pendingTaskCount":0,"activeTaskCount":0}}`
	tests := []struct {
		name    string
		missing string
	}{
		{name: "accepts required nulls"},
		{name: "rejects missing details", missing: "operationStatusDetails"},
		{name: "rejects missing health", missing: "health"},
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
			if tt.missing != "" {
				require.ErrorContains(t, err, "required property "+tt.missing)
				return
			}
			require.NoError(t, err)
			require.Len(t, racks, 1)
			require.True(t, racks[0].OperationStatusDetails.IsSet())
			require.Nil(t, racks[0].OperationStatusDetails.Get())
			require.True(t, racks[0].Health.IsSet())
			require.Nil(t, racks[0].Health.Get())
		})
	}
}
