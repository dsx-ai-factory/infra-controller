// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPITenant(t *testing.T) {
	dbtn := &cdbm.Tenant{
		ID:             uuid.New(),
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Org Display name"),
		Created:        time.Now(),
		Updated:        time.Now(),
	}

	deprecations := []APIDeprecation{}
	for _, deprecation := range tenantCapabilityDeprecations {
		deprecations = append(deprecations, NewAPIDeprecation(deprecation))
	}

	tests := []struct {
		name                     string
		targetedInstanceCreation bool
		expectedCapabilitiesJSON string
	}{
		{
			name:                     "includes enabled compatibility capability",
			targetedInstanceCreation: true,
			expectedCapabilitiesJSON: `{"targetedInstanceCreation":true}`,
		},
		{
			name:                     "omits disabled compatibility capability",
			targetedInstanceCreation: false,
			expectedCapabilitiesJSON: `{}`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPITenant(dbtn, tt.targetedInstanceCreation)
			assert.Equal(t, &APITenant{
				ID:             dbtn.ID.String(),
				Org:            dbtn.Org,
				OrgDisplayName: dbtn.OrgDisplayName,
				Capabilities:   tenantToAPITenantCapabilities(tt.targetedInstanceCreation),
				Deprecations:   deprecations,
				Created:        dbtn.Created,
				Updated:        dbtn.Updated,
			}, got)

			body, err := json.Marshal(got)
			require.NoError(t, err)
			var response map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(body, &response))
			assert.JSONEq(t, tt.expectedCapabilitiesJSON, string(response["capabilities"]))
			require.Len(t, got.Deprecations, 1)
			assert.Equal(t, time.Date(2026, time.October, 1, 0, 0, 0, 0, time.UTC), got.Deprecations[0].TakeActionBy)
		})
	}
}

func TestAPITenantRoutingProfile_FromProto(t *testing.T) {
	coreResponse := &corev1.FindTenantResponse{
		Tenant:                       &corev1.Tenant{RoutingProfileType: cutil.GetPtr("INTERNAL")},
		PermittedRoutingProfileTypes: []string{"EXTERNAL", "INTERNAL", "MAINTENANCE"},
	}

	t.Run("allows site-scoped alternatives", func(t *testing.T) {
		response := &APITenantRoutingProfile{}
		response.FromProto(coreResponse, true)

		assert.Equal(t, "internal", response.DefaultRoutingProfile)
		assert.Equal(t, []string{"external", "internal", "MAINTENANCE"}, response.PermittedRoutingProfiles)
	})

	t.Run("limits selection to tenant default without privilege", func(t *testing.T) {
		response := &APITenantRoutingProfile{}
		response.FromProto(coreResponse, false)

		assert.Equal(t, "internal", response.DefaultRoutingProfile)
		assert.Equal(t, []string{"internal"}, response.PermittedRoutingProfiles)
	})

	t.Run("keeps response keys stable for missing tenant", func(t *testing.T) {
		response := &APITenantRoutingProfile{}
		response.FromProto(&corev1.FindTenantResponse{}, true)

		body, err := json.Marshal(response)
		require.NoError(t, err)
		assert.JSONEq(t, `{"defaultRoutingProfile":"","permittedRoutingProfiles":[]}`, string(body))
	})
}

func TestNewAPITenantSummary(t *testing.T) {
	dbtn := &cdbm.Tenant{
		ID:             uuid.New(),
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Org Display name"),
		Created:        time.Now(),
		Updated:        time.Now(),
	}

	type args struct {
		dbtn *cdbm.Tenant
	}
	tnAPITenantSummary := APITenantSummary{
		Org:            dbtn.Org,
		OrgDisplayName: dbtn.OrgDisplayName,
		Capabilities:   tenantToAPITenantCapabilities(false),
	}
	for _, deprecation := range tenantCapabilityDeprecations {
		tnAPITenantSummary.Deprecations = append(tnAPITenantSummary.Deprecations, NewAPIDeprecation(deprecation))
	}

	tests := []struct {
		name string
		args args
		want *APITenantSummary
	}{
		{
			name: "test init API summary model for Tenant",
			args: args{
				dbtn: dbtn,
			},
			want: &tnAPITenantSummary,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPITenantSummary(tt.args.dbtn)
			assert.Equal(t, tt.want, got)

			body, err := json.Marshal(got)
			require.NoError(t, err)
			var response map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(body, &response))
			assert.JSONEq(t, `{}`, string(response["capabilities"]))
		})
	}
}
