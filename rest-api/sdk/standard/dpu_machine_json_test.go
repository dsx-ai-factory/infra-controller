// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestDpuMachineSummary_UnmarshalJSON(t *testing.T) {
	payload := []byte(`{
		"id":"dpu-1",
		"infrastructureProviderId":"00000000-0000-0000-0000-000000000001",
		"siteId":"00000000-0000-0000-0000-000000000002",
		"hostMachineId":"host-1",
		"dpuAgentVersion":"",
		"bmcInfo":null,
		"dmiData":null,
		"interfaces":null,
		"softwareComponents":null,
		"health":null,
		"labels":null,
		"state":"",
		"dpuNetworkConfig":null,
		"lastRebooted":null,
		"placementInRack":null
	}`)

	t.Run("accepts and preserves required nulls", func(t *testing.T) {
		var summary DpuMachineSummary
		require.NoError(t, json.Unmarshal(payload, &summary))

		encoded, err := json.Marshal(summary)
		require.NoError(t, err)

		var response map[string]any
		require.NoError(t, json.Unmarshal(encoded, &response))
		for _, field := range []string{
			"bmcInfo",
			"dmiData",
			"interfaces",
			"softwareComponents",
			"health",
			"labels",
			"dpuNetworkConfig",
			"lastRebooted",
			"placementInRack",
		} {
			require.Contains(t, response, field)
			require.Nil(t, response[field])
		}
	})

	t.Run("rejects a missing required nullable field", func(t *testing.T) {
		var response map[string]any
		require.NoError(t, json.Unmarshal(payload, &response))
		delete(response, "dpuNetworkConfig")

		missingNetworkConfig, err := json.Marshal(response)
		require.NoError(t, err)

		var summary DpuMachineSummary
		require.ErrorContains(t, json.Unmarshal(missingNetworkConfig, &summary), "required property dpuNetworkConfig")
	})

	t.Run("rejects a missing required non-nullable field", func(t *testing.T) {
		var summary DpuMachineSummary
		require.ErrorContains(t, json.Unmarshal([]byte(`{"dpuNetworkConfig":null}`), &summary), "required property id")
	})
}
