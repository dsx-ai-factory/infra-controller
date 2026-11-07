// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package testing provides shared constants and utilities for cloud-auth tests
package testing

import (
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/Nerzal/gocloak/v13"
)

// Test domain constants - consolidate similar patterns across test files
const (
	// Email domains
	TestOrgDomain = "testorg.com"
	NvidiaDomain  = "nvidia.com"
	ExampleDomain = "example.com"
	DevDomain     = "test.com"

	// Base URLs and endpoints
	LocalKeycloakURL = "http://localhost:8082"
	TestKeycloakURL  = "https://keycloak.test.com"
	TestCallbackURL  = "http://localhost:3000/callback"

	// Client identifiers
	TestClientID     = "test-client"
	TestClientSecret = "test-secret"
	AdminClientID    = "admin-cli"

	// Organization names
	TestOrgName     = "test-org"
	NICoDevOrgName  = "nico-tenant-dev"
	NICoProviderOrg = "nico-prime-provider"
	NvidiaOrgName   = "nvidia"

	// User identifiers
	TestUserEmail     = "john.doe@testorg.com"
	AdminUserEmail    = "admin@nvidia.com"
	TestUserSubject   = "test-subject"
	TestUserFirstName = "John"
	TestUserLastName  = "Doe"

	// Common JWT claims
	TestIssuer   = "test-issuer"
	TestAudience = "ngc"

	// Keycloak realm and IDP constants
	TestRealm       = "nico"
	TestIDPAlias    = "testorg-idp"
	TestIDPProvider = "oidc"
)

// Test role constants
const (
	NICoProviderAdminRole  = authz.ProviderAdminRole
	NICoTenantAdminRole    = authz.TenantAdminRole
	NICoProviderViewerRole = authz.ProviderViewerRole
	NICoTenantViewerRole   = "TENANT_VIEWER"
)

// Key generation constants
const (
	TestRSAKeySize   = 2048
	TestECDSACurve   = "P-256"
	TestKeyID        = "test-key-id"
	TestSigningKeyID = "signing-key-1"
)

// Mock IDP configurations for reuse across tests
var (
	TestIDPConfig = map[string]string{
		"clientId":          "test-client-id",
		"clientSecret":      "test-client-secret",
		"authorizationUrl":  "https://auth.testorg.com/oauth2/authorize",
		"tokenUrl":          "https://auth.testorg.com/oauth2/token",
		"userInfoUrl":       "https://auth.testorg.com/oauth2/userinfo",
		"jwksUrl":           "https://auth.testorg.com/.well-known/jwks.json",
		"issuer":            "https://auth.testorg.com",
		"validateSignature": "true",
		"useJwksUrl":        "true",
		"pkceEnabled":       "false",
		"emailDomain":       TestOrgDomain,
	}

	// Standard test IDP representation
	StandardTestIDP = &gocloak.IdentityProviderRepresentation{
		Alias:       gocloak.StringP(TestIDPAlias),
		DisplayName: gocloak.StringP("TestOrg OIDC"),
		ProviderID:  gocloak.StringP(TestIDPProvider),
		Enabled:     gocloak.BoolP(true),
		Config:      &TestIDPConfig,
	}
)

// Common test emails for different scenarios
var TestEmails = struct {
	Valid     []string
	EdgeCases []string
	Invalid   []string
}{
	Valid: []string{
		"user@" + TestOrgDomain,
		"admin@" + NvidiaDomain,
		"test.user@" + ExampleDomain,
		"first.last@" + DevDomain,
	},
	EdgeCases: []string{
		"user+tag@" + TestOrgDomain,
		"user.with.dots@" + TestOrgDomain,
		"user-with-dashes@" + TestOrgDomain,
		"UPPERCASE@" + TestOrgDomain,
	},
	Invalid: []string{
		"invalid-email",
		"@" + TestOrgDomain,
		"user@",
		"",
		"user space@" + TestOrgDomain,
	},
}

// Common realm access role combinations for testing
var TestRealmRoles = struct {
	SingleOrg     []string
	MultiOrg      []string
	MixedCase     []string
	InvalidFormat []string
}{
	SingleOrg: []string{
		TestOrgName + ":" + NICoProviderAdminRole,
		TestOrgName + ":" + NICoTenantAdminRole,
	},
	MultiOrg: []string{
		TestOrgName + ":" + NICoProviderAdminRole,
		NICoDevOrgName + ":" + NICoTenantAdminRole,
		NvidiaOrgName + ":" + NICoProviderViewerRole,
	},
	MixedCase: []string{
		"TestOrg:" + NICoProviderAdminRole,
		"TESTORG:" + NICoTenantAdminRole,
	},
	InvalidFormat: []string{
		"invalid-role-format",
		":" + NICoProviderAdminRole, // Empty org
		TestOrgName + ":",           // Empty role
		"",                          // Empty string
	},
}
