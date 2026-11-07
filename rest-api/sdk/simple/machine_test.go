// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestMachineFromStandardIncludesScoutVersion(t *testing.T) {
	version := "2.6.1"
	apiMachine := standard.NewMachine()
	apiMachine.SetScoutVersion(version)

	tests := []struct {
		name       string
		apiMachine standard.Machine
		want       *string
	}{
		{
			name:       "returns reported version",
			apiMachine: *apiMachine,
			want:       &version,
		},
		{
			name:       "returns nil when version is unknown",
			apiMachine: *standard.NewMachine(),
			want:       nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			machine := machineFromStandard(tt.apiMachine)

			assert.Equal(t, tt.want, machine.ScoutVersion)

			response, err := json.Marshal(machine)
			require.NoError(t, err)

			var fields map[string]interface{}
			require.NoError(t, json.Unmarshal(response, &fields))
			assert.NotContains(t, fields, "lastScoutObservedVersion")
			_, found := fields["scoutVersion"]
			assert.True(t, found)
		})
	}
}
