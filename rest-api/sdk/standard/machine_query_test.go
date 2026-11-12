// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"net/http"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestApiGetAllMachineRequest_QuerySerialization(t *testing.T) {
	tests := []struct {
		name  string
		apply func(ApiGetAllMachineRequest) ApiGetAllMachineRequest
		query string
	}{
		{name: "omitted filters", query: "pageNumber=1"},
		{
			name: "empty IDs",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.Id([]string{})
			},
			query: "pageNumber=1",
		},
		{
			name: "single ID",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.Id([]string{"ID1"})
			},
			query: "id=ID1&pageNumber=1",
		},
		{
			name: "repeated IDs",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.Id([]string{"ID1", "ID2"})
			},
			query: "id=ID1&id=ID2&pageNumber=1",
		},
		{
			name: "repeated Instance Type IDs",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.InstanceTypeId([]string{"type1", "type2"})
			},
			query: "instanceTypeId=type1&instanceTypeId=type2&pageNumber=1",
		},
		{
			name: "repeated Tenant IDs",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.TenantId([]string{"tenant1", "tenant2"})
			},
			query: "pageNumber=1&tenantId=tenant1&tenantId=tenant2",
		},
		{
			name: "repeated Statuses",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.Status([]string{"Ready", "Error"})
			},
			query: "pageNumber=1&status=Ready&status=Error",
		},
		{
			name: "repeated Capability Names with reserved characters",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.CapabilityName([]string{"GPU A", "GPU,B&C"})
			},
			query: "capabilityName=GPU+A&capabilityName=GPU%2CB%26C&pageNumber=1",
		},
		{
			name: "repeated hardware SKU Device Types",
			apply: func(r ApiGetAllMachineRequest) ApiGetAllMachineRequest {
				return r.HwSkuDeviceType([]string{"gpu", "cpu"})
			},
			query: "hwSkuDeviceType=gpu&hwSkuDeviceType=cpu&pageNumber=1",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			transport := &captureExpectedInventoryTransport{}
			cfg := NewConfiguration()
			cfg.HTTPClient = &http.Client{Transport: transport}
			request := NewAPIClient(cfg).MachineAPI.GetAllMachine(context.Background(), "test-org")
			if tt.apply != nil {
				request = tt.apply(request)
			}

			_, _, err := request.Execute()
			require.NoError(t, err)
			require.NotNil(t, transport.req)
			assert.Equal(t, tt.query, transport.req.URL.RawQuery)
		})
	}
}
