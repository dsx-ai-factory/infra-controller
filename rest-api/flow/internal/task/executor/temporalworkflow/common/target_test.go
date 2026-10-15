// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/converter"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestTarget_Validate(t *testing.T) {
	tests := []struct {
		name       string
		target     Target
		want       []string
		wantUseMAC bool
		wantError  string
	}{
		{
			name: "complete IDs preserve existing path",
			target: Target{Type: devicetypes.ComponentTypeCompute,
				IdentifierType: IdentifierTypeManagerID,
				Identifiers:    []string{"machine-1", "machine-2"}},
			want: []string{"machine-1", "machine-2"},
		},
		{
			name: "mixed ingestion uses one MAC batch",
			target: Target{Type: devicetypes.ComponentTypeCompute,
				IdentifierType: IdentifierTypeMACAddress,
				Identifiers:    []string{"aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"}},
			want:       []string{"aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"},
			wantUseMAC: true,
		},
		{
			name: "incomplete MAC batch rejected",
			target: Target{Type: devicetypes.ComponentTypeCompute,
				IdentifierType: IdentifierTypeMACAddress,
				Identifiers:    []string{"aa:bb:cc:dd:ee:01", ""}},
			wantUseMAC: true,
			want:       []string{"aa:bb:cc:dd:ee:01", ""},
			wantError:  "identifiers must not be empty",
		},
		{
			name:      "unknown type rejected",
			target:    Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: "invalid", Identifiers: []string{"machine-1"}},
			want:      []string{"machine-1"},
			wantError: "unknown component ID type",
		},
		{
			name:      "empty target rejected",
			target:    Target{Type: devicetypes.ComponentTypeCompute},
			wantError: "component IDs or MAC addresses are required",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantUseMAC, test.target.UsesMACAddresses())
			assert.Equal(t, test.want, test.target.Identifiers)
			err := test.target.Validate()
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestTarget_TemporalPayload(t *testing.T) {
	// This is the Target wire shape on main before identifier types existed.
	type legacyTarget struct {
		Type         devicetypes.ComponentType
		ComponentIDs []string
	}
	// Old activity payloads remain readable. Their readiness IDs are ignored:
	// MAC readiness is now resolved from current inventory, not a saved ID list.
	type typedTarget struct {
		Type                  devicetypes.ComponentType
		ComponentIDType       string `json:",omitempty"`
		ComponentIDs          []string
		ReadinessComponentIDs []string `json:",omitempty"`
	}
	for _, tc := range []struct {
		name  string
		input any
		want  Target
	}{
		{"legacy", legacyTarget{devicetypes.ComponentTypeCompute, []string{"machine-1"}}, Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"machine-1"}}},
		{"MAC", typedTarget{devicetypes.ComponentTypeCompute, "mac_address", []string{"aa:bb:cc:dd:ee:01"}, []string{"machine-1"}}, Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: IdentifierTypeMACAddress, Identifiers: []string{"aa:bb:cc:dd:ee:01"}}},
		{"manager ID", typedTarget{Type: devicetypes.ComponentTypeCompute, ComponentIDType: "manager_id", ComponentIDs: []string{"machine-1"}}, Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: IdentifierTypeManagerID, Identifiers: []string{"machine-1"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dc := converter.GetDefaultDataConverter()
			payload, err := dc.ToPayload(tc.input)
			require.NoError(t, err)
			var got Target
			require.NoError(t, dc.FromPayload(payload, &got))
			require.Equal(t, tc.want, got)
			require.NoError(t, got.Validate())
			reencoded, err := dc.ToPayload(got)
			require.NoError(t, err)
			if tc.name != "MAC" {
				assert.JSONEq(t, string(payload.Data), string(reencoded.Data))
			}
			var old typedTarget
			require.NoError(t, dc.FromPayload(reencoded, &old))
			assert.Equal(t, got.Identifiers, old.ComponentIDs)
			assert.Equal(t, string(got.IdentifierType), old.ComponentIDType)
		})
	}
}
