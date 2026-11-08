// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"net/http"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestValidateOrgMembership(t *testing.T) {
	for _, tc := range []struct {
		name    string
		user    *cdbm.User
		allowed bool
	}{
		{name: "missing user"},
		{name: "roles in another org do not grant membership", user: &cdbm.User{OrgData: cdbm.OrgData{
			"other-org": {Roles: []string{authz.ProviderAdminRole}},
		}}},
		{name: "membership is case insensitive and independent of roles", user: &cdbm.User{OrgData: cdbm.OrgData{
			"TARGET-ORG": {Name: "TARGET-ORG"},
		}}, allowed: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			membership, apiError := ValidateOrgMembership(zerolog.Nop(), "target-org", tc.user)
			if tc.allowed {
				require.Nil(t, apiError)
				require.NotNil(t, membership.org)
				assert.Equal(t, "target-org", membership.orgName)
				return
			}
			require.NotNil(t, apiError)
			assert.Equal(t, http.StatusForbidden, apiError.Code)
			assert.Equal(t, "Failed to validate membership for org: target-org", apiError.Message)
			assert.Nil(t, membership.org)
		})
	}
}

func TestOrgMembership_IsProviderOrTenant(t *testing.T) {
	viewer, apiError := ValidateOrgMembership(zerolog.Nop(), "target-org", &cdbm.User{OrgData: cdbm.OrgData{
		"target-org": {Roles: []string{authz.ProviderViewerRole}},
	}})
	require.Nil(t, apiError)
	for _, tc := range []struct {
		name       string
		membership OrgMembership
		message    string
	}{
		{name: "zero value cannot bypass membership", message: "Failed to validate membership for org"},
		{name: "membership alone does not grant an admin role", membership: viewer, message: "User does not have Provider or Tenant role with org"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			provider, tenant, apiError := tc.membership.IsProviderOrTenant(context.Background(), zerolog.Nop(), nil, false, nil)
			require.NotNil(t, apiError)
			assert.Equal(t, http.StatusForbidden, apiError.Code)
			assert.Equal(t, tc.message, apiError.Message)
			assert.Nil(t, provider)
			assert.Nil(t, tenant)
		})
	}
}

func TestIsProviderOrTenant(t *testing.T) {
	provider, tenant, apiError := IsProviderOrTenant(context.Background(), zerolog.Nop(), nil, "target-org", &cdbm.User{}, false, nil)
	require.NotNil(t, apiError)
	assert.Equal(t, http.StatusForbidden, apiError.Code)
	assert.Equal(t, "Failed to validate membership for org", apiError.Message)
	assert.Nil(t, provider)
	assert.Nil(t, tenant)
}
