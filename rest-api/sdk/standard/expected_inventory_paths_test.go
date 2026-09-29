// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestExpectedInventoryReplaceAllPaths(t *testing.T) {
	tests := []struct {
		name    string
		path    string
		execute func(*APIClient) (*http.Response, error)
	}{
		{
			name: "Expected Rack uses explicit all path",
			path: "/v2/org/test-org/nico/expected-rack/all",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackAPI.ReplaceAllExpectedRack(context.Background(), "test-org").
					ExpectedRackList(ExpectedRackList{}).Execute()
				return response, err
			},
		},
		{
			name: "Expected Rack legacy method remains compatible",
			path: "/v2/org/test-org/nico/expected-rack",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackAPI.ReplaceAllExpectedRackLegacy(context.Background(), "test-org").
					ExpectedRackList(ExpectedRackList{}).Execute()
				return response, err
			},
		},
		{
			name: "Expected Rack Group uses explicit all path",
			path: "/v2/org/test-org/nico/expected-rack-group/all",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackGroupAPI.ReplaceAllExpectedRackGroup(context.Background(), "test-org").
					ExpectedRackGroupList(ExpectedRackGroupList{}).Execute()
				return response, err
			},
		},
		{
			name: "Expected Rack Group legacy method remains compatible",
			path: "/v2/org/test-org/nico/expected-rack-group",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackGroupAPI.ReplaceAllExpectedRackGroupLegacy(context.Background(), "test-org").
					ExpectedRackGroupList(ExpectedRackGroupList{}).Execute()
				return response, err
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			transport := &captureExpectedInventoryTransport{}
			cfg := NewConfiguration()
			cfg.Servers = ServerConfigurations{{URL: "https://example.com", Description: "test"}}
			cfg.HTTPClient = &http.Client{Transport: transport}

			response, err := tt.execute(NewAPIClient(cfg))

			require.NoError(t, err)
			require.NotNil(t, response)
			require.NotNil(t, transport.req)
			assert.Equal(t, http.MethodPut, transport.req.Method)
			assert.Equal(t, tt.path, transport.req.URL.Path)
		})
	}
}

type captureExpectedInventoryTransport struct {
	req *http.Request
}

func (t *captureExpectedInventoryTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	t.req = req.Clone(req.Context())
	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"application/json"}},
		Body:       io.NopCloser(strings.NewReader("[]")),
	}, nil
}
