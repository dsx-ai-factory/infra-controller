// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
	"github.com/stretchr/testify/require"
)

func TestIpBlockManager_GetIpBlocks(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.Equal(t, "/v2/org/test-org/nico/ipblock", r.URL.Path)
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("X-Pagination", `{"pageNumber":1,"pageSize":3,"total":3}`)
		_, _ = io.WriteString(w, `[{"id":"allocated","managed":true},{"id":"private","managed":false},{"id":"legacy"}]`)
	}))
	defer server.Close()
	cfg := standard.NewConfiguration()
	cfg.Servers = standard.ServerConfigurations{{URL: server.URL}}
	client := &Client{apiClient: standard.NewAPIClient(cfg), apiMetadata: ApiMetadata{Organization: "test-org"}, Logger: NewNoOpLogger()}
	blocks, _, err := NewIpBlockManager(client).GetIpBlocks(context.Background(), nil)
	require.Nil(t, err)
	require.Len(t, blocks, 3)
	for i, row := range []struct{ name, managed string }{
		{"allocated", "true"}, {"private", "false"}, {"legacy", "null"},
	} {
		t.Run(row.name, func(t *testing.T) {
			require.Equal(t, row.name, blocks[i].ID)
			encoded, err := json.Marshal(blocks[i])
			require.NoError(t, err)
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(encoded, &fields))
			require.Equal(t, row.managed, string(fields["managed"]))
		})
	}
}
