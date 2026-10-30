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

const taskListTestSiteID = "550e8400-e29b-41d4-a716-446655440000"
const taskListTestPolicyID = "63f1c140-81c9-4711-8c3a-1664aaed2cd9"

func TestTaskAPIService_GetAllTasks(t *testing.T) {
	testTaskListSDKRequest(t, "/task", func(ctx context.Context, client *APIClient, filtered bool) error {
		request := client.TaskAPI.GetAllTasks(ctx, "test-org").SiteId(taskListTestSiteID)
		if filtered {
			request = request.EventPolicyId(taskListTestPolicyID).EventType(EVENTTYPE_HARDWARE_LEAK_DETECTED).
				EventTriggeredOnly(true).ActiveOnly(true).IncludeReport(true).PageNumber(2).PageSize(1)
		}
		_, _, err := request.Execute()
		return err
	})
}

func TestRackAPIService_GetRackTasks(t *testing.T) {
	testTaskListSDKRequest(t, "/rack/rack-17/task", func(ctx context.Context, client *APIClient, filtered bool) error {
		request := client.RackAPI.GetRackTasks(ctx, "test-org", "rack-17").SiteId(taskListTestSiteID)
		if filtered {
			request = request.EventPolicyId(taskListTestPolicyID).EventType(EVENTTYPE_HARDWARE_LEAK_DETECTED).
				EventTriggeredOnly(true).ActiveOnly(true).IncludeReport(true).PageNumber(2).PageSize(1)
		}
		_, _, err := request.Execute()
		return err
	})
}

func TestTrayAPIService_GetTrayTasks(t *testing.T) {
	testTaskListSDKRequest(t, "/tray/tray-17/task", func(ctx context.Context, client *APIClient, filtered bool) error {
		request := client.TrayAPI.GetTrayTasks(ctx, "test-org", "tray-17").SiteId(taskListTestSiteID)
		if filtered {
			request = request.EventPolicyId(taskListTestPolicyID).EventType(EVENTTYPE_HARDWARE_LEAK_DETECTED).
				EventTriggeredOnly(true).ActiveOnly(true).IncludeReport(true).PageNumber(2).PageSize(1)
		}
		_, _, err := request.Execute()
		return err
	})
}

func testTaskListSDKRequest(t *testing.T, path string, execute func(context.Context, *APIClient, bool) error) {
	t.Helper()
	for _, tc := range []struct {
		name     string
		filtered bool
		query    url.Values
	}{
		{
			name:  "ordinary lists omit event selectors",
			query: url.Values{"siteId": {taskListTestSiteID}, "activeOnly": {"false"}, "includeReport": {"false"}, "pageNumber": {"1"}, "pageSize": {"20"}},
		},
		{
			name:     "event filters combine with target and report options",
			filtered: true,
			query:    url.Values{"siteId": {taskListTestSiteID}, "eventPolicyId": {taskListTestPolicyID}, "eventType": {"hardware.leak.detected"}, "eventTriggeredOnly": {"true"}, "activeOnly": {"true"}, "includeReport": {"true"}, "pageNumber": {"2"}, "pageSize": {"1"}},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			captureErr := errors.New("request captured")
			transport := &queryCaptureTransport{err: captureErr}
			cfg := NewConfiguration()
			cfg.HTTPClient = &http.Client{Transport: transport}
			err := execute(t.Context(), NewAPIClient(cfg), tc.filtered)
			require.ErrorIs(t, err, captureErr)
			require.NotNil(t, transport.request)
			assert.Equal(t, http.MethodGet, transport.request.Method)
			assert.Equal(t, "/v2/org/test-org/nico"+path, transport.request.URL.Path)
			assert.Equal(t, tc.query, transport.request.URL.Query())
		})
	}
}
