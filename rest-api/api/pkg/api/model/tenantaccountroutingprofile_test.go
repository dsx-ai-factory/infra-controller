// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPITenantAccountRoutingProfileUpdateRequest_Validate(t *testing.T) {
	cases := []struct {
		name  string
		body  string
		valid bool
	}{
		{
			name:  "configured name",
			body:  `{"siteId":"497f6eca-6276-4993-bfeb-53cbbbba6f08","defaultRoutingProfile":"TENANT_EDGE"}`,
			valid: true,
		},
		{
			name: "missing site",
			body: `{"defaultRoutingProfile":"internal"}`,
		},
		{
			name: "invalid site",
			body: `{"siteId":"invalid","defaultRoutingProfile":"internal"}`,
		},
		{
			name: "omitted profile",
			body: `{"siteId":"497f6eca-6276-4993-bfeb-53cbbbba6f08"}`,
		},
		{
			name: "null profile cannot clear default",
			body: `{"siteId":"497f6eca-6276-4993-bfeb-53cbbbba6f08","defaultRoutingProfile":null}`,
		},
		{
			name: "empty profile cannot clear default",
			body: `{"siteId":"497f6eca-6276-4993-bfeb-53cbbbba6f08","defaultRoutingProfile":""}`,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var req APITenantAccountRoutingProfileUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(tc.body), &req))
			assert.Equal(t, tc.valid, req.Validate() == nil)
		})
	}
}

func TestAPITenantAccountRoutingProfileUpdateRequest_ToProto(t *testing.T) {
	cases := []struct {
		profile string
		want    string
	}{
		{
			profile: "internal",
			want:    "INTERNAL",
		},
		{
			profile: "TENANT_EDGE",
			want:    "TENANT_EDGE",
		},
	}
	for _, tc := range cases {
		t.Run(tc.profile, func(t *testing.T) {
			current := &corev1.Tenant{
				OrganizationId: "tenant-org",
				Metadata: &corev1.Metadata{
					Name: "Tenant's existing name",
				},
				Version: "V7-T1761856992374052",
			}
			req := APITenantAccountRoutingProfileUpdateRequest{
				DefaultRoutingProfile: tc.profile,
			}
			got := req.ToProto(current)
			assert.Equal(t, current.OrganizationId, got.OrganizationId)
			assert.Equal(t, current.Metadata, got.Metadata)
			assert.Equal(t, &current.Version, got.IfVersionMatch)
			assert.Equal(t, tc.want, got.GetRoutingProfileType())
			assert.Nil(t, current.RoutingProfileType)
		})
	}
}
