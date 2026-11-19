// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestClient_GetNVLinkLogicalPartition(t *testing.T) {
	tests := []struct {
		name   string
		body   string
		siteID string
	}{
		{name: "returns partition site", body: `{"siteId":"site-2"}`, siteID: "site-2"},
		{name: "omitted site", body: `{}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client := newJSONResponseClient(t, tt.body)
			partition, apiErr := client.GetNVLinkLogicalPartition(t.Context(), "partition-1")
			require.Nil(t, apiErr)
			require.NotNil(t, partition)
			assert.Equal(t, tt.siteID, partition.SiteID)
		})
	}
}
