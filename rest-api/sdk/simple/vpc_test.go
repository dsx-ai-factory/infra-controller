// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestClient_GetVpc(t *testing.T) {
	tests := []struct {
		name   string
		body   string
		siteID string
		labels map[string]string
		status string
	}{
		{
			name:   "returns site, labels and status",
			body:   `{"siteId":"site-2","labels":{"nvlink_partition_id":"partition-1","env":"dev"},"status":"Deleting"}`,
			siteID: "site-2",
			labels: map[string]string{"nvlink_partition_id": "partition-1", "env": "dev"},
			status: "Deleting",
		},
		{
			name: "omitted metadata",
			body: `{}`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client := newJSONResponseClient(t, tt.body)
			vpc, apiErr := client.GetVpc(t.Context(), "vpc-1")
			require.Nil(t, apiErr)
			require.NotNil(t, vpc)
			assert.Equal(t, tt.siteID, vpc.SiteID)
			assert.Equal(t, tt.labels, vpc.Labels)
			assert.Equal(t, tt.status, vpc.Status)
		})
	}
}
