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
		"interfaces":[],
		"softwareComponents":[],
		"health":null,
		"labels":{},
		"state":"",
		"dpuNetworkConfig":null,
		"lastRebooted":null,
		"placementInRack":null,
		"lldpNeighbors":[]
	}`)

	t.Run("accepts and preserves explicit nulls and empty collections", func(t *testing.T) {
		var summary DpuMachineSummary
		require.NoError(t, json.Unmarshal(payload, &summary))

		encoded, err := json.Marshal(summary)
		require.NoError(t, err)

		var response map[string]any
		require.NoError(t, json.Unmarshal(encoded, &response))
		for _, field := range []string{
			"bmcInfo",
			"dmiData",
			"health",
			"dpuNetworkConfig",
			"lastRebooted",
			"placementInRack",
		} {
			require.Contains(t, response, field)
			require.Nil(t, response[field])
		}
		require.Equal(t, []any{}, response["interfaces"])
		require.Equal(t, []any{}, response["softwareComponents"])
		require.Equal(t, map[string]any{}, response["labels"])
		require.Equal(t, []any{}, response["lldpNeighbors"])
	})

	t.Run("accepts an omitted optional nullable field", func(t *testing.T) {
		var response map[string]any
		require.NoError(t, json.Unmarshal(payload, &response))
		delete(response, "dpuNetworkConfig")

		withoutNetworkConfig, err := json.Marshal(response)
		require.NoError(t, err)

		var summary DpuMachineSummary
		require.NoError(t, json.Unmarshal(withoutNetworkConfig, &summary))
		require.False(t, summary.DpuNetworkConfig.IsSet())
	})

	t.Run("rejects a null required collection field", func(t *testing.T) {
		var response map[string]any
		require.NoError(t, json.Unmarshal(payload, &response))
		response["interfaces"] = nil

		nullInterfaces, err := json.Marshal(response)
		require.NoError(t, err)

		var summary DpuMachineSummary
		require.ErrorContains(t, json.Unmarshal(nullInterfaces, &summary), "required property interfaces")
	})

	t.Run("rejects a missing required non-nullable field", func(t *testing.T) {
		var summary DpuMachineSummary
		require.ErrorContains(t, json.Unmarshal([]byte(`{"dpuNetworkConfig":null}`), &summary), "required property id")
	})
}
