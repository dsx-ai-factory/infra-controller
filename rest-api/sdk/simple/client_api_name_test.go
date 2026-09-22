// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestClient_Authenticate(t *testing.T) {
	tests := []struct {
		name              string
		metadataVersion   string
		normalizedVersion string
		compatible        bool
	}{
		{
			name:              "unprefixed minimum API version",
			metadataVersion:   "0.2.86",
			normalizedVersion: "v0.2.86",
			compatible:        true,
		},
		{
			name:              "prefixed prerelease API version",
			metadataVersion:   "v2.3.0-pr-16-g663042ae7",
			normalizedVersion: "v2.3.0-pr-16-g663042ae7",
			compatible:        true,
		},
		{
			name:              "prefixed API version below minimum",
			metadataVersion:   "v0.2.85",
			normalizedVersion: "v0.2.85",
			compatible:        false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			visitedPaths := map[string]int{}

			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				visitedPaths[r.URL.Path]++
				w.Header().Set("Content-Type", "application/json")

				switch r.URL.Path {
				case "/v2/org/test-org/nico/metadata":
					_, _ = io.WriteString(w, `{"version":"`+tt.metadataVersion+`"}`)
				case "/v2/org/test-org/nico/infrastructure-provider/current":
					_, _ = io.WriteString(w, `{"id":"provider-1"}`)
				case "/v2/org/test-org/nico/tenant/current":
					_, _ = io.WriteString(w, `{"id":"tenant-1"}`)
				case "/v2/org/test-org/nico/site":
					_, _ = io.WriteString(w, `[{"id":"site-1","name":"site-1"}]`)
				case "/v2/org/test-org/nico/vpc":
					_, _ = io.WriteString(w, `[]`)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			client, err := NewClient(ClientConfig{
				BaseURL: server.URL,
				Org:     "test-org",
				APIName: "nico",
				Token:   "test-token",
				Logger:  NewNoOpLogger(),
			})
			require.NoError(t, err)

			err = client.Authenticate(context.Background())
			if tt.compatible {
				require.NoError(t, err)
			} else {
				var apiErr *ApiError
				require.ErrorAs(t, err, &apiErr)
				assert.Equal(t, http.StatusUpgradeRequired, apiErr.Code)
			}

			version, compatible := client.IsMinimumAPIVersion("v0.2.86")
			assert.Equal(t, tt.normalizedVersion, version)
			assert.Equal(t, tt.compatible, compatible)

			assert.Equal(t, 1, visitedPaths["/v2/org/test-org/nico/metadata"])
			assert.Equal(t, 1, visitedPaths["/v2/org/test-org/nico/infrastructure-provider/current"])
			assert.Equal(t, 1, visitedPaths["/v2/org/test-org/nico/tenant/current"])
			assert.Equal(t, 1, visitedPaths["/v2/org/test-org/nico/site"])
			assert.Equal(t, 1, visitedPaths["/v2/org/test-org/nico/vpc"])
			assert.Zero(t, visitedPaths["/v2/org/test-org/carbide/metadata"])
		})
	}
}

func TestNewClientFromEnvReadsAPIName(t *testing.T) {
	t.Setenv("NICO_BASE_URL", "https://example.com")
	t.Setenv("NICO_ORG", "test-org")
	t.Setenv("NICO_API_NAME", "nico")
	t.Setenv("NICO_TOKEN", "test-token")

	client, err := NewClientFromEnv()
	require.NoError(t, err)

	assert.Equal(t, "https://example.com", client.Config.BaseURL)
	assert.Equal(t, "test-org", client.Config.Org)
	assert.Equal(t, "nico", client.Config.APIName)
	assert.Equal(t, "test-token", client.Config.Token)
}
