// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestDpuMachineUnmarshalJSON(t *testing.T) {
	// "observed_at" is an additive server field absent from OpenAPI.
	const response = `{
		"id": "dpu-1",
		"infrastructureProviderId": "provider-1",
		"siteId": "site-1",
		"hostMachineId": "host-1",
		"dmiData": {"productSerial": "dpu-serial-1"},
		"health": {
			"observedAt": "2026-01-01T00:00:00Z",
			"observed_at": "2026-01-01T00:00:00Z"
		},
		"state": "Ready",
		"dpuNetworkConfig": {
			"asn": 4268052792,
			"tenantHostAsn": 4268052792,
			"vniDevice": "dpu-vni",
			"managedHostConfigVersion": "v1",
			"useAdminNetwork": true,
			"remoteId": "remote-1",
			"vpcIsolationBehavior": "Strict",
			"statefulAclsEnabled": true,
			"enableDhcp": true,
			"isPrimaryDpu": true,
			"datacenterAsn": 64513
		}
	}`

	t.Run("allows additive fields", func(t *testing.T) {
		var machine DpuMachine
		require.NoError(t, json.Unmarshal([]byte(response), &machine))
		require.Equal(t, "dpu-1", machine.Id)
		require.Equal(t, "dpu-serial-1", machine.DmiData.GetProductSerial())
		require.Equal(t, "2026-01-01T00:00:00Z", machine.Health.GetObservedAt())
		require.Equal(t, "Ready", machine.State)
		require.Equal(t, uint32(4268052792), machine.DpuNetworkConfig.Asn)
		require.Equal(t, uint32(4268052792), *machine.DpuNetworkConfig.TenantHostAsn.Get())
		require.Equal(t, "dpu-vni", machine.DpuNetworkConfig.VniDevice)
	})

	t.Run("requires schema fields", func(t *testing.T) {
		withoutState := strings.Replace(response, "\t\t\"state\": \"Ready\",\n", "", 1)

		var machine DpuMachine
		require.Error(t, json.Unmarshal([]byte(withoutState), &machine))
	})
}
