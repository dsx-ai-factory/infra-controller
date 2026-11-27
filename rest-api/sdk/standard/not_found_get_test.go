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

func TestGetNotFoundErrorDecoding(t *testing.T) {
	const message = "Could not find resource with specified ID"
	tests := []struct {
		name    string
		execute func(*APIClient) (*http.Response, error)
	}{
		{
			name: "Site",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.SiteAPI.GetSite(context.Background(), "test-org", "missing-site").Execute()
				return response, err
			},
		},
		{
			name: "Operating System",
			execute: func(client *APIClient) (*http.Response, error) {
				_, response, err := client.OperatingSystemAPI.GetOperatingSystem(context.Background(), "test-org", "missing-os").Execute()
				return response, err
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := NewConfiguration()
			cfg.Servers = ServerConfigurations{{URL: "https://example.com"}}
			cfg.HTTPClient = &http.Client{Transport: notFoundTransport{}}

			response, err := tt.execute(NewAPIClient(cfg))
			require.NotNil(t, response)
			assert.Equal(t, http.StatusNotFound, response.StatusCode)
			var apiError *GenericOpenAPIError
			require.ErrorAs(t, err, &apiError)
			model, ok := apiError.Model().(NICoAPIError)
			require.True(t, ok, "404 response must decode as NICoAPIError")
			assert.Equal(t, "nico", model.GetSource())
			assert.Equal(t, message, model.GetMessage())
		})
	}
}

type notFoundTransport struct{}

func (notFoundTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	return &http.Response{
		StatusCode: http.StatusNotFound,
		Status:     "404 Not Found",
		Header:     http.Header{"Content-Type": []string{"application/json"}},
		Body:       io.NopCloser(strings.NewReader(`{"source":"nico","message":"Could not find resource with specified ID","data":null}`)),
		Request:    request,
	}, nil
}
