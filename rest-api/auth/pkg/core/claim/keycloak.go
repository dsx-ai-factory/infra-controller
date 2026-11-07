// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package claim

import (
	"strings"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/rs/zerolog/log"

	"github.com/golang-jwt/jwt/v5"
)

// RealmAccess represents the realm_access structure in Keycloak JWT
type RealmAccess struct {
	Roles []string `json:"roles"`
}

// KeycloakClaims represents the structure of Keycloak JWT claims
type KeycloakClaims struct {
	Email       string      `json:"email"`
	FirstName   string      `json:"given_name"`
	LastName    string      `json:"family_name"`
	RealmAccess RealmAccess `json:"realm_access"`
	ClientId    string      `json:"client_id"`
	Oidc_Id     string      `json:"oidc_id"`
	jwt.RegisteredClaims
}

// GetClientId returns the client_id from KeycloakClaims
func (k *KeycloakClaims) GetClientId() string {
	return k.ClientId
}

// GetOidcId returns the oidc_id from KeycloakClaims
func (k *KeycloakClaims) GetOidcId() string {
	return k.Oidc_Id
}

// GetEmail returns the email from KeycloakClaims
func (k *KeycloakClaims) GetEmail() string {
	return k.Email
}

// GetRealmRoles returns the realm roles from KeycloakClaims
func (k *KeycloakClaims) GetRealmRoles() []string {
	roles := k.RealmAccess.Roles
	return roles
}

// ToOrgData parses realm roles and returns a map of organizations to their roles
// Roles are deduplicated and empty org names or roles are skipped
func (k *KeycloakClaims) ToOrgData() cdbm.OrgData {
	realmRoles := k.GetRealmRoles()
	if len(realmRoles) == 0 {
		log.Warn().Msg("ToOrgData: No realm roles found! This will result in empty orgData")
		return cdbm.OrgData{}
	}

	orgData := cdbm.OrgData{}

	for _, roleStr := range realmRoles {
		parts := strings.Split(roleStr, ":")
		if len(parts) == 2 {
			orgName := strings.TrimSpace(parts[0])
			orgName = strings.ToLower(orgName)
			role := strings.TrimSpace(parts[1])

			// Skip empty org names or roles
			if orgName == "" || role == "" {
				continue
			}

			// If this org already exists in the map, add role if not already present
			if org, ok := orgData[orgName]; ok {
				// Check if role already exists to avoid duplicates
				roleExists := false
				for _, existingRole := range org.Roles {
					if existingRole == role {
						roleExists = true
						break
					}
				}
				if !roleExists {
					org.Roles = append(org.Roles, role)
					orgData[orgName] = org
				}
			} else {
				newOrg := cdbm.Org{
					ID:          0, // Default ID since not available in JWT claims
					Name:        orgName,
					DisplayName: orgName,      // Use orgName as display name since not available in JWT
					OrgType:     "ENTERPRISE", // Default org type
					Roles:       []string{role},
					Teams:       []cdbm.Team{}, // Initialize empty teams slice
				}
				orgData[orgName] = newOrg
			}
		}
	}
	return orgData
}
