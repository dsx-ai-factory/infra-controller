// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// TestDpuExtensionServiceFromStandard verifies simplified responses preserve
// placement fields because callers must distinguish baseline services from instance-scoped services.
func TestDpuExtensionServiceFromStandard(t *testing.T) {
	// Older responses can expose a target without the new deployment policy field.
	target := standard.DPUEXTENSIONSERVICEDPUTARGET_ALL_ACTIVE
	apiService := standard.DpuExtensionService{}
	apiService.DpuTarget.Set(&target)

	service := dpuExtensionServiceFromStandard(apiService)

	// Preserve the known target without inventing a policy absent from the server response.
	assert.Empty(t, service.DeploymentType)
	require.NotNil(t, service.DpuTarget)
	assert.Equal(t, DpuExtensionServiceDpuTargetAllActive, *service.DpuTarget)
}

// TestDpuExtensionServiceManager_Create verifies the simple SDK sends baseline
// intent over HTTP and retains server defaults when older callers omit the policy.
func TestDpuExtensionServiceManager_Create(t *testing.T) {
	baseline := DpuExtensionServiceDeploymentTypeBaseline
	for _, tc := range []struct {
		name        string
		policy      *string
		want        string
		serviceType string
		data        string
	}{
		// Existing callers must omit the new field and observe the server's demand-driven default.
		{name: "omitted policy", want: DpuExtensionServiceDeploymentTypeDemandDriven,
			serviceType: DpuExtensionServiceTypeKubernetesPod,
			data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: baseline\nspec:\n  containers:\n  - name: service\n    image: busybox:latest"},
		// Baseline must survive both the outbound SDK conversion and the response conversion.
		{name: "baseline policy", policy: &baseline, want: DpuExtensionServiceDeploymentTypeBaseline,
			serviceType: DpuExtensionServiceTypeDpfHelmChart,
			data:        `{"repoURL":"https://example.com/charts","chartName":"chart","chartVersion":"1.0.0","serviceID":"chart-v1","deployInCluster":false,"security":{"privileged":false}}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			// Observe actual JSON rather than only testing the intermediate generated request.
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, http.MethodPost, r.Method)
				var body map[string]any
				if !assert.NoError(t, json.NewDecoder(r.Body).Decode(&body)) {
					w.WriteHeader(http.StatusBadRequest)
					return
				}
				if tc.policy == nil {
					assert.NotContains(t, body, "deploymentType")
				} else {
					assert.Equal(t, *tc.policy, body["deploymentType"])
				}
				assert.NotContains(t, body, "dpuTarget")
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusCreated)
				assert.NoError(t, json.NewEncoder(w).Encode(map[string]any{
					"id": "baseline-service", "name": "baseline", "serviceType": tc.serviceType,
					"deploymentType": tc.want, "dpuTarget": nil,
				}))
			}))
			defer server.Close()

			// Exercise the public SDK entry point and verify the server's accepted placement is returned.
			client := newSimpleTestClient(server.URL)
			got, apiErr := NewDpuExtensionServiceManager(client).Create(context.Background(), DpuExtensionServiceCreateRequest{
				Name: "baseline", ServiceType: tc.serviceType,
				DeploymentType: tc.policy, Data: tc.data,
			})
			require.Nil(t, apiErr)
			require.NotNil(t, got)
			assert.Equal(t, tc.want, got.DeploymentType)
			assert.Nil(t, got.DpuTarget)
		})
	}
}
