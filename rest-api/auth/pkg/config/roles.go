// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	authz "github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/core"
	"github.com/golang-jwt/jwt/v5"
)

// =============================================================================
// Role Constants and Variables
// =============================================================================

var (
	// ServiceAccountRoles are the default roles assigned to service accounts
	ServiceAccountRoles = []string{authz.ProviderAdminRole, authz.TenantAdminRole}

	// AllowedRoles is the set of valid roles that can be assigned to users.
	// Both static roles in config and dynamic roles from claims must be from this set.
	AllowedRoles = map[string]bool{
		authz.TenantAdminRole:   true,
		authz.ProviderAdminRole: true,
	}
)

// =============================================================================
// Role Validation Functions
// =============================================================================

// validateRoles checks that all roles are in the AllowedRoles set.
// Returns false immediately upon finding the first invalid role.
func validateRoles(roles []string) bool {
	for _, role := range roles {
		if !AllowedRoles[role] {
			return false
		}
	}
	return true
}

// IsValidRole checks if a single role is in the AllowedRoles set.
func IsValidRole(role string) bool {
	return AllowedRoles[role]
}

// FilterToAllowedRoles filters a list of roles to only include allowed roles.
// Returns core.ErrInvalidRole if no valid roles remain after filtering.
func FilterToAllowedRoles(roles []string) (allowed []string, err error) {
	for _, role := range roles {
		if AllowedRoles[role] {
			allowed = append(allowed, role)
		}
	}
	if len(allowed) == 0 {
		return nil, core.ErrInvalidRole
	}
	return allowed, nil
}

// =============================================================================
// Role Extraction Functions
// =============================================================================

// GetRolesFromAttribute extracts roles from a nested claim attribute and filters to allowed roles.
// Returns nil if the attribute doesn't exist or contains no valid roles.
func GetRolesFromAttribute(claims jwt.MapClaims, attribute string) ([]string, error) {
	value := core.GetClaimAttribute(claims, attribute)
	if value == nil {
		return nil, nil
	}

	roles, err := core.InterfaceToStringSlice(value)
	if err != nil {
		return nil, err
	}
	return FilterToAllowedRoles(roles)
}
