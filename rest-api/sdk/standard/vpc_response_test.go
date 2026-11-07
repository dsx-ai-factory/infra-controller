// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestVPCResponseDecodesSlaacEnabled(t *testing.T) {
	disabled := false
	enabled := true
	tests := []struct {
		name                      string
		networkVirtualizationType string
		slaacEnabled              *bool
	}{
		{name: "disabled Ethernet VPC", networkVirtualizationType: "ETHERNET_VIRTUALIZER", slaacEnabled: &disabled},
		{name: "enabled FNN VPC", networkVirtualizationType: "FNN", slaacEnabled: &enabled},
		{name: "older response without SLAAC support", networkVirtualizationType: "FNN"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			slaacField := ""
			if tt.slaacEnabled != nil {
				slaacField = fmt.Sprintf(`"slaacEnabled":%t,`, *tt.slaacEnabled)
			}
			response := fmt.Sprintf(`{
				"id":"497f6eca-6276-4993-bfeb-53cbbbba6f08",
				"name":"spark-vpc",
				"description":"Virtual network for machines executing Spark jobs",
				"org":"xskkpgqpeakn",
				"tenantId":"34f5c98e-f430-457b-a812-92637d0c6fd0",
				"siteId":"72771e6a-6f5e-4de4-a5b9-1266c4197811",
				"controllerVpcId":"497f6eca-6276-4993-bfeb-53cbbbba6f08",
				"networkVirtualizationType":%q,
				%s
				"routingProfile":null,
				"routingProfileOverrides":null,
				"requestedVni":12001,
				"vni":12001,
				"networkSecurityGroupId":null,
				"networkSecurityGroupPropagationDetails":null,
				"nvLinkLogicalPartitionId":null,
				"labels":{"region":"us-west-1","env":"dev"},
				"status":"Ready",
				"statusHistory":[],
				"created":"2019-08-24T14:15:22Z",
				"updated":"2019-08-24T14:15:22Z"
			}`, tt.networkVirtualizationType, slaacField)

			decoded := &VPC{}
			err := json.Unmarshal([]byte(response), decoded)
			require.NoError(t, err)
			assert.Equal(t, tt.slaacEnabled, decoded.SlaacEnabled)
			assert.Equal(t, "497f6eca-6276-4993-bfeb-53cbbbba6f08", decoded.GetId())
			assert.Equal(t, tt.networkVirtualizationType, decoded.GetNetworkVirtualizationType())
		})
	}
}
