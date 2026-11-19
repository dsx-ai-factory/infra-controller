// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"net/http"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestClient_GetNVLinkLogicalPartition(t *testing.T) {
	tests := []struct {
		name    string
		body    string
		siteID  string
		wantErr bool
	}{
		{
			name:   "returns partition site",
			body:   `{"id":"partition-1","name":"partition-1","siteId":"site-2","tenantId":"tenant-1","status":"Ready","statusHistory":[],"created":"2026-01-15T12:00:00Z","updated":"2026-01-15T12:00:00Z"}`,
			siteID: "site-2",
		},
		{
			name:    "returns error when response misses required fields",
			body:    `{}`,
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client := newJSONResponseClient(t, tt.body)
			partition, apiErr := client.GetNVLinkLogicalPartition(t.Context(), "partition-1")
			if tt.wantErr {
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusOK, apiErr.Code)
				assert.Nil(t, partition)
				return
			}
			require.Nil(t, apiErr)
			require.NotNil(t, partition)
			assert.Equal(t, tt.siteID, partition.SiteID)
		})
	}
}
