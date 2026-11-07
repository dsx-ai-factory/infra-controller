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

func TestSetAPINameRewritesGeneratedRequests(t *testing.T) {
	transport := &captureTransport{}

	cfg := NewConfiguration()
	cfg.Servers = ServerConfigurations{
		{URL: "https://example.com", Description: "test"},
	}
	cfg.HTTPClient = &http.Client{Transport: transport}
	cfg.SetAPIName("nico")

	client := NewAPIClient(cfg)
	_, _, err := client.MetadataAPI.GetMetadata(context.Background(), "test-org").Execute()
	require.NoError(t, err)

	require.NotNil(t, transport.req)
	assert.Equal(t, "/v2/org/test-org/nico/metadata", transport.req.URL.Path)
	assert.Equal(t, "nico", cfg.GetAPIName())
}

type captureTransport struct {
	req *http.Request
}

func (t *captureTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	reqCopy := req.Clone(req.Context())
	urlCopy := *req.URL
	reqCopy.URL = &urlCopy
	t.req = reqCopy

	return &http.Response{
		StatusCode: http.StatusOK,
		Header: http.Header{
			"Content-Type": []string{"application/json"},
		},
		Body: io.NopCloser(strings.NewReader(`{"version":"0.2.86"}`)),
	}, nil
}
