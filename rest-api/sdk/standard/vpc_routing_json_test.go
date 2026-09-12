// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestVpcRoutingResponses_UnmarshalJSON(t *testing.T) {
	tests := []struct {
		name           string
		payload        string
		newResponse    func() any
		nullableFields []string
	}{
		{
			name:           "routing state",
			payload:        `{"vpcId":"497f6eca-6276-4993-bfeb-53cbbbba6f08","version":"V7-T1761856992374052","routingProfile":null,"activeVni":10100,"retainedAllocation":null}`,
			newResponse:    func() any { return &VpcRoutingState{} },
			nullableFields: []string{"routingProfile", "retainedAllocation"},
		},
		{
			name:           "inactive VNI release",
			payload:        `{"vpcId":"497f6eca-6276-4993-bfeb-53cbbbba6f08","version":"V9-T1761856992374054","routingProfile":null,"activeVni":51000,"releasedInactiveVni":10100}`,
			newResponse:    func() any { return &VpcInactiveVniReleaseResult{} },
			nullableFields: []string{"routingProfile"},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Run("accepts and preserves required nulls", func(t *testing.T) {
				response := tt.newResponse()
				require.NoError(t, json.Unmarshal([]byte(tt.payload), response))
				encoded, err := json.Marshal(response)
				require.NoError(t, err)
				var fields map[string]any
				require.NoError(t, json.Unmarshal(encoded, &fields))
				for _, field := range tt.nullableFields {
					require.Contains(t, fields, field)
					require.Nil(t, fields[field])
				}
			})
			for _, field := range append([]string{"vpcId"}, tt.nullableFields...) {
				t.Run("rejects missing "+field, func(t *testing.T) {
					var fields map[string]any
					require.NoError(t, json.Unmarshal([]byte(tt.payload), &fields))
					delete(fields, field)
					encoded, err := json.Marshal(fields)
					require.NoError(t, err)
					require.ErrorContains(t, json.Unmarshal(encoded, tt.newResponse()), "required property "+field)
				})
			}
		})
	}
}
