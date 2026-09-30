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

func TestExpectedInventoryBulkPaths(t *testing.T) {
	const siteID = "f97df110-f4de-492e-8849-4a6af68026b0"
	tests := []struct {
		name    string
		method  string
		path    string
		body    string
		execute func(*APIClient) (*http.Response, error)
	}{
		{
			name:   "Expected Rack uses explicit all path",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-rack/all",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackAPI.ReplaceAllExpectedRack(context.Background(), "test-org").
					ExpectedRackList(ExpectedRackList{}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Rack legacy method remains compatible",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-rack",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackAPI.ReplaceAllExpectedRackLegacy(context.Background(), "test-org").
					ExpectedRackList(ExpectedRackList{}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Rack Group uses explicit all path",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-rack-group/all",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackGroupAPI.ReplaceAllExpectedRackGroup(context.Background(), "test-org").
					ExpectedRackGroupList(ExpectedRackGroupList{}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Rack Group legacy method remains compatible",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-rack-group",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedRackGroupAPI.ReplaceAllExpectedRackGroupLegacy(context.Background(), "test-org").
					ExpectedRackGroupList(ExpectedRackGroupList{}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Machine replace-all uses explicit all path",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-machine/all",
			body:   `{"siteId":"f97df110-f4de-492e-8849-4a6af68026b0","expectedMachines":[]}`,
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedMachineAPI.ReplaceAllExpectedMachine(context.Background(), "test-org").
					ExpectedMachineList(ExpectedMachineList{SiteId: siteID, ExpectedMachines: []ExpectedMachineCreateRequest{}}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Machine delete-all uses explicit all path",
			method: http.MethodDelete,
			path:   "/v2/org/test-org/nico/expected-machine/all",
			execute: func(client *APIClient) (*http.Response, error) {
				return client.ExpectedMachineAPI.DeleteAllExpectedMachine(context.Background(), "test-org").SiteId("site-id").Execute()
			},
		},
		{
			name:   "Expected Switch replace-all uses explicit all path",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-switch/all",
			body:   `{"siteId":"f97df110-f4de-492e-8849-4a6af68026b0","expectedSwitches":[]}`,
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedSwitchAPI.ReplaceAllExpectedSwitch(context.Background(), "test-org").
					ExpectedSwitchList(ExpectedSwitchList{SiteId: siteID, ExpectedSwitches: []ExpectedSwitchCreateRequest{}}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Switch delete-all uses explicit all path",
			method: http.MethodDelete,
			path:   "/v2/org/test-org/nico/expected-switch/all",
			execute: func(client *APIClient) (*http.Response, error) {
				return client.ExpectedSwitchAPI.DeleteAllExpectedSwitch(context.Background(), "test-org").SiteId("site-id").Execute()
			},
		},
		{
			name:   "Expected Power Shelf replace-all uses explicit all path",
			method: http.MethodPut,
			path:   "/v2/org/test-org/nico/expected-power-shelf/all",
			body:   `{"siteId":"f97df110-f4de-492e-8849-4a6af68026b0","expectedPowerShelves":[]}`,
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.ExpectedPowerShelfAPI.ReplaceAllExpectedPowerShelf(context.Background(), "test-org").
					ExpectedPowerShelfList(ExpectedPowerShelfList{SiteId: siteID, ExpectedPowerShelves: []ExpectedPowerShelfCreateRequest{}}).Execute()
				return response, err
			},
		},
		{
			name:   "Expected Power Shelf delete-all uses explicit all path",
			method: http.MethodDelete,
			path:   "/v2/org/test-org/nico/expected-power-shelf/all",
			execute: func(client *APIClient) (*http.Response, error) {
				return client.ExpectedPowerShelfAPI.DeleteAllExpectedPowerShelf(context.Background(), "test-org").SiteId("site-id").Execute()
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
			assert.Equal(t, tt.method, transport.req.Method)
			assert.Equal(t, tt.path, transport.req.URL.Path)
			if tt.body != "" {
				assert.JSONEq(t, tt.body, string(transport.body))
			}
		})
	}
}

type captureExpectedInventoryTransport struct {
	req  *http.Request
	body []byte
}

func (t *captureExpectedInventoryTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.Body != nil {
		var err error
		t.body, err = io.ReadAll(req.Body)
		if err != nil {
			return nil, err
		}
	}
	t.req = req.Clone(req.Context())
	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"application/json"}},
		Body:       io.NopCloser(strings.NewReader("[]")),
	}, nil
}
