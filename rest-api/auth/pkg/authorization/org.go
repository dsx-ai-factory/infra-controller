// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authz

import (
	"strings"

	"github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/roles"
	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
)

// Role-name constants are sourced from common/pkg/roles so they can be
// referenced from packages that cannot import auth/pkg/authorization
// (e.g. db model tests, which would create an import cycle, and the
// workflow production image, which does not ship the auth module).
const (
	// ProviderAdminRole is the role that gives Provider Admin access to an org
	ProviderAdminRole = roles.ProviderAdminRole
	// ProviderViewerRole is the role that gives Provider Viewer access to an org
	ProviderViewerRole = roles.ProviderViewerRole
	// TenantAdminRole is the role that gives Tenant Admin access to an org
	TenantAdminRole = roles.TenantAdminRole
)

// ValidateOrgMembership validates if a given user is member of an org
func ValidateOrgMembership(user *cdbm.User, org string) (bool, error) {
	_, err := user.OrgData.GetOrgByName(org)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return false, nil
		}
		return false, err
	}
	return true, nil
}

// ValidateUserRoles validates user roles using the appropriate method based on user data
func ValidateUserRoles(user *cdbm.User, orgName string, teamName *string, targetRoles ...string) bool {
	userOrgDetails, err := user.OrgData.GetOrgByName(orgName)
	if err != nil {
		return false
	}
	return ValidateUserRolesInOrg(*userOrgDetails, teamName, targetRoles...)
}

// matchesTargetRole reports whether role matches any entry in targetRoleMap.
// It first checks the role as-is (new unprefixed form, e.g. "PROVIDER_ADMIN"), then
// checks again after stripping the first word and its underscore (legacy prefixed form,
// e.g. "NICO_PROVIDER_ADMIN" or "FORGE_PROVIDER_ADMIN" → "PROVIDER_ADMIN").
// This allows a gradual transition while both old and new role values can exist in
// Keycloak / the DB simultaneously.
// TODO: remove the stripped fallback once all stored roles have been migrated to the
// new unprefixed form (i.e. no "NICO_*" or "FORGE_*" role values remain).
// NOTE: callers must pass the package-level constants (ProviderAdminRole, etc.) as
// targetRoles — not hand-written string copies — so that the comparison is always
// against the canonical unprefixed form.
func matchesTargetRole(role string, targetRoleMap map[string]bool) bool {
	if targetRoleMap[role] {
		return true
	}
	// Strip the first word and underscore and try again.
	if idx := strings.Index(role, "_"); idx != -1 {
		return targetRoleMap[role[idx+1:]]
	}
	return false
}

// ValidateUserRolesInOrg checks if user has any of the specified roles (not all).
// targetRoles must be the package-level constants (ProviderAdminRole, etc.).
func ValidateUserRolesInOrg(userOrgDetails cdbm.Org, teamName *string, targetRoles ...string) bool {
	var userHasRole bool

	targetRoleMap := map[string]bool{}
	for _, targetRole := range targetRoles {
		targetRoleMap[targetRole] = true
	}

	if teamName == nil {
		// Check if user has an org level role.
		for _, userOrgRole := range userOrgDetails.Roles {
			if matchesTargetRole(userOrgRole, targetRoleMap) {
				userHasRole = true
				break
			}
		}
	} else {
		// Check if user has a team role.
		for _, userTeamDetails := range userOrgDetails.Teams {
			if userTeamDetails.Name != *teamName {
				continue
			}

			for _, userTeamRole := range userTeamDetails.Roles {
				if matchesTargetRole(userTeamRole, targetRoleMap) {
					userHasRole = true
					break
				}
			}
		}
	}

	return userHasRole
}
