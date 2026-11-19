// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"errors"
	"net/http"
	"net/url"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRepeatableGetQuerySerialization(t *testing.T) {
	ctx := context.Background()
	tests := []struct {
		name    string
		path    string
		execute func(*APIClient) error
		query   url.Values
	}{
		{
			name: "verify repeatable query parameters are serialized correctly",
			path: "/v2/org/test-org/nico/allocation",
			execute: func(client *APIClient) error {
				_, _, err := client.AllocationAPI.GetAllAllocation(ctx, "test-org").
					ConstraintType([]string{}).
					ConstraintValue([]int32{10, 20}).
					Id(nil).
					IncludeRelation([]string{"InfrastructureProvider", "Tenant"}).
					ResourceType([]string{"InstanceType", "IPBlock"}).
					ResourceTypeId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready"}).
					Execute()
				return err
			},
			query: url.Values{
				"constraintValue": {"10", "20"},
				"includeRelation": {"InfrastructureProvider", "Tenant"},
				"pageNumber":      {"1"},
				"resourceType":    {"InstanceType", "IPBlock"},
				"resourceTypeId":  {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"siteId":          {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":          {"Ready"},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			captureErr := errors.New("request captured")
			transport := &queryCaptureTransport{
				err: captureErr,
			}
			cfg := NewConfiguration()
			cfg.HTTPClient = &http.Client{
				Transport: transport,
			}

			err := tt.execute(NewAPIClient(cfg))

			require.ErrorIs(t, err, captureErr)
			require.NotNil(t, transport.request)
			assert.Equal(t, http.MethodGet, transport.request.Method)
			assert.Equal(t, tt.path, transport.request.URL.Path)
			assert.Equal(t, tt.query, transport.request.URL.Query())
		})
	}
}

type queryCaptureTransport struct {
	request *http.Request
	err     error
}

func (t *queryCaptureTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	t.request = request.Clone(request.Context())
	return nil, t.err
}
