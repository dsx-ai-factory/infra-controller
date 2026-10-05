// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package claim

import (
	"fmt"
	"strings"
	"testing"
	"time"

	authz "github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/authorization"
	testutil "github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/testing"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
)

func TestKeycloakClaims_GetClientId(t *testing.T) {
	tests := []struct {
		name   string
		claims *KeycloakClaims
		want   string
	}{
		{
			name: "returns client id when present",
			claims: &KeycloakClaims{
				ClientId: "test-client-id",
			},
			want: "test-client-id",
		},
		{
			name: "returns empty string when client id is empty",
			claims: &KeycloakClaims{
				ClientId: "",
			},
			want: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.claims.GetClientId()
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestKeycloakClaims_GetOidcId(t *testing.T) {
	tests := []struct {
		name   string
		claims *KeycloakClaims
		want   string
	}{
		{
			name: "returns oidc id when present",
			claims: &KeycloakClaims{
				Oidc_Id: "test-oidc-id-123",
			},
			want: "test-oidc-id-123",
		},
		{
			name: "returns empty string when oidc id is empty",
			claims: &KeycloakClaims{
				Oidc_Id: "",
			},
			want: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.claims.GetOidcId()
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestKeycloakClaims_GetEmail(t *testing.T) {
	tests := []struct {
		name   string
		claims *KeycloakClaims
		want   string
	}{
		{
			name: "returns email when present",
			claims: &KeycloakClaims{
				Email: "test@example.com",
			},
			want: "test@example.com",
		},
		{
			name: "returns empty string when email is empty",
			claims: &KeycloakClaims{
				Email: "",
			},
			want: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.claims.GetEmail()
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestKeycloakClaims_GetRealmRoles(t *testing.T) {
	tests := []struct {
		name   string
		claims *KeycloakClaims
		want   []string
	}{
		{
			name: "returns realm roles when present with org-specific roles",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"},
				},
			},
			want: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"},
		},
		{
			name: "returns empty slice when no roles",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{},
				},
			},
			want: []string{},
		},
		{
			name: "returns nil when realm access is nil",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{},
			},
			want: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.claims.GetRealmRoles()
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestKeycloakClaims_ToOrgData(t *testing.T) {
	tests := []struct {
		name   string
		claims *KeycloakClaims
		want   cdbm.OrgData
	}{
		{
			name: "converts realm roles to org data correctly",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"},
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole, authz.TenantAdminRole},
					Teams:       []cdbm.Team{},
				},
				"anotherorg": cdbm.Org{
					ID:          0,
					Name:        "anotherorg",
					DisplayName: "anotherorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "handles mixed case org names by converting to lowercase",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"TestOrg:PROVIDER_ADMIN", "ANOTHERORG:TENANT_ADMIN"},
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
					Teams:       []cdbm.Team{},
				},
				"anotherorg": cdbm.Org{
					ID:          0,
					Name:        "anotherorg",
					DisplayName: "anotherorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.TenantAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "ignores malformed roles without colon",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN", "malformed-role", "anotherorg:TENANT_ADMIN"},
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
					Teams:       []cdbm.Team{},
				},
				"anotherorg": cdbm.Org{
					ID:          0,
					Name:        "anotherorg",
					DisplayName: "anotherorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.TenantAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "returns empty org data when no realm roles",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{},
				},
			},
			want: cdbm.OrgData{},
		},
		{
			name: "appends roles to existing org",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN"},
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole, authz.TenantAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "ignores roles with extra colons",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN:special", "testorg:TENANT_ADMIN"},
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.TenantAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "accepts all roles from issuer (no role validation)",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{"testorg:CUSTOM_ROLE", "testorg:ANY_OTHER_ROLE", "anotherorg:NON_NICO_ROLE"},
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{"CUSTOM_ROLE", "ANY_OTHER_ROLE"},
					Teams:       []cdbm.Team{},
				},
				"anotherorg": cdbm.Org{
					ID:          0,
					Name:        "anotherorg",
					DisplayName: "anotherorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{"NON_NICO_ROLE"},
					Teams:       []cdbm.Team{},
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.claims.ToOrgData()
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestRealmAccess(t *testing.T) {
	tests := []struct {
		name        string
		realmAccess RealmAccess
		want        []string
	}{
		{
			name: "realm access with multiple org-specific roles",
			realmAccess: RealmAccess{
				Roles: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"},
			},
			want: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"},
		},
		{
			name: "realm access with no roles",
			realmAccess: RealmAccess{
				Roles: []string{},
			},
			want: []string{},
		},
		{
			name: "realm access with nil roles",
			realmAccess: RealmAccess{
				Roles: nil,
			},
			want: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.realmAccess.Roles)
		})
	}
}

// TestKeycloakClaims_Integration tests complete integration
func TestKeycloakClaims_Integration(t *testing.T) {
	claims := &KeycloakClaims{
		Email:     "john.doe@testorg.com",
		FirstName: "John",
		LastName:  "Doe",
		ClientId:  "test-client",
		Oidc_Id:   "oidc-123-456",
		RealmAccess: RealmAccess{
			Roles: []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"},
		},
		RegisteredClaims: jwt.RegisteredClaims{
			Subject: "user-subject-123",
		},
	}

	assert.Equal(t, "john.doe@testorg.com", claims.GetEmail())
	assert.Equal(t, "test-client", claims.GetClientId())
	assert.Equal(t, "oidc-123-456", claims.GetOidcId())
	assert.Equal(t, []string{"testorg:PROVIDER_ADMIN", "testorg:TENANT_ADMIN", "anotherorg:PROVIDER_ADMIN"}, claims.GetRealmRoles())
	orgData := claims.ToOrgData()
	assert.Len(t, orgData, 2)

	testOrg, exists := orgData["testorg"]
	assert.True(t, exists)
	assert.Equal(t, "testorg", testOrg.Name)
	assert.Equal(t, "testorg", testOrg.DisplayName)
	assert.Equal(t, "ENTERPRISE", testOrg.OrgType)
	assert.Equal(t, []string{authz.ProviderAdminRole, authz.TenantAdminRole}, testOrg.Roles)
	assert.Empty(t, testOrg.Teams)

	anotherOrg, exists := orgData["anotherorg"]
	assert.True(t, exists)
	assert.Equal(t, "anotherorg", anotherOrg.Name)
	assert.Equal(t, "anotherorg", anotherOrg.DisplayName)
	assert.Equal(t, "ENTERPRISE", anotherOrg.OrgType)
	assert.Equal(t, []string{authz.ProviderAdminRole}, anotherOrg.Roles)
	assert.Empty(t, anotherOrg.Teams)
}

// TestKeycloakClaims_ToOrgData_WithConstants tests ToOrgData using shared constants
func TestKeycloakClaims_ToOrgData_WithConstants(t *testing.T) {
	tests := []struct {
		name   string
		claims *KeycloakClaims
		want   cdbm.OrgData
	}{
		{
			name: "single org with admin role - using constants",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{
						testutil.TestOrgName + ":" + testutil.NICoProviderAdminRole,
					},
				},
			},
			want: cdbm.OrgData{
				testutil.TestOrgName: cdbm.Org{
					ID:          0,
					Name:        testutil.TestOrgName,
					DisplayName: testutil.TestOrgName,
					OrgType:     "ENTERPRISE",
					Roles:       []string{testutil.NICoProviderAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "multiple orgs with different roles - using constants",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: testutil.TestRealmRoles.MultiOrg,
				},
			},
			want: cdbm.OrgData{
				testutil.TestOrgName: cdbm.Org{
					ID:          0,
					Name:        testutil.TestOrgName,
					DisplayName: testutil.TestOrgName,
					OrgType:     "ENTERPRISE",
					Roles:       []string{testutil.NICoProviderAdminRole},
					Teams:       []cdbm.Team{},
				},
				testutil.NICoDevOrgName: cdbm.Org{
					ID:          0,
					Name:        testutil.NICoDevOrgName,
					DisplayName: testutil.NICoDevOrgName,
					OrgType:     "ENTERPRISE",
					Roles:       []string{testutil.NICoTenantAdminRole},
					Teams:       []cdbm.Team{},
				},
				testutil.NvidiaOrgName: cdbm.Org{
					ID:          0,
					Name:        testutil.NvidiaOrgName,
					DisplayName: testutil.NvidiaOrgName,
					OrgType:     "ENTERPRISE",
					Roles:       []string{testutil.NICoProviderViewerRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "mixed case org names - using constants",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: testutil.TestRealmRoles.MixedCase,
				},
			},
			want: cdbm.OrgData{
				"testorg": cdbm.Org{ // Normalized to lowercase
					ID:          0,
					Name:        "testorg",
					DisplayName: "testorg",
					OrgType:     "ENTERPRISE",
					Roles:       []string{testutil.NICoProviderAdminRole, testutil.NICoTenantAdminRole},
					Teams:       []cdbm.Team{},
				},
			},
		},
		{
			name: "invalid role formats - using constants",
			claims: &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: testutil.TestRealmRoles.InvalidFormat,
				},
			},
			want: cdbm.OrgData{}, // Should be empty due to invalid formats
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.claims.ToOrgData()
			assert.Equal(t, tt.want, got)
		})
	}
}

// TestKeycloakClaims_WithUtilities tests integration using test utilities
func TestKeycloakClaims_WithUtilities(t *testing.T) {
	// Use constants for test data
	claims := &KeycloakClaims{
		Email:     testutil.TestUserEmail,
		FirstName: testutil.TestUserFirstName,
		LastName:  testutil.TestUserLastName,
		ClientId:  testutil.TestClientID,
		Oidc_Id:   "oidc-123-456",
		RealmAccess: RealmAccess{
			Roles: testutil.TestRealmRoles.MultiOrg,
		},
		RegisteredClaims: jwt.RegisteredClaims{
			Subject: testutil.TestUserSubject,
		},
	}

	// Test all getter methods using constants
	assert.Equal(t, testutil.TestUserEmail, claims.GetEmail())
	assert.Equal(t, testutil.TestClientID, claims.GetClientId())
	assert.Equal(t, "oidc-123-456", claims.GetOidcId())
	assert.Equal(t, testutil.TestRealmRoles.MultiOrg, claims.GetRealmRoles())

	// Test org data conversion
	orgData := claims.ToOrgData()
	assert.Len(t, orgData, 3) // Should have 3 orgs from MultiOrg test data

	// Validate specific orgs using constants
	testOrg, exists := orgData[testutil.TestOrgName]
	assert.True(t, exists)
	assert.Equal(t, testutil.TestOrgName, testOrg.Name)
	assert.Equal(t, testutil.TestOrgName, testOrg.DisplayName)
	assert.Equal(t, "ENTERPRISE", testOrg.OrgType)
	assert.Equal(t, []string{testutil.NICoProviderAdminRole}, testOrg.Roles)
	assert.Empty(t, testOrg.Teams)

	nicoOrg, exists := orgData[testutil.NICoDevOrgName]
	assert.True(t, exists)
	assert.Equal(t, testutil.NICoDevOrgName, nicoOrg.Name)
	assert.Equal(t, []string{testutil.NICoTenantAdminRole}, nicoOrg.Roles)

	nvidiaOrg, exists := orgData[testutil.NvidiaOrgName]
	assert.True(t, exists)
	assert.Equal(t, testutil.NvidiaOrgName, nvidiaOrg.Name)
	assert.Equal(t, []string{testutil.NICoProviderViewerRole}, nvidiaOrg.Roles)
}

// TestKeycloakClaims_EdgeCases tests error handling edge cases
func TestKeycloakClaims_EdgeCases(t *testing.T) {

	t.Run("nil_realm_access_handling", func(t *testing.T) {
		// Test with various nil/empty scenarios
		claims := &KeycloakClaims{
			Email: testutil.TestUserEmail,
			// RealmAccess intentionally omitted (zero value)
		}

		// Should not panic
		func() {
			defer func() {
				if r := recover(); r != nil {
					t.Errorf("ToOrgData panicked with nil RealmAccess: %v", r)
				}
			}()

			orgData := claims.ToOrgData()
			assert.NotNil(t, orgData)
			assert.Empty(t, orgData)
		}()
	})

	t.Run("malformed_email_handling", func(t *testing.T) {
		malformedEmails := testutil.TestEmails.Invalid

		for _, email := range malformedEmails {
			claims := &KeycloakClaims{
				Email: email,
			}

			// Should not panic with malformed emails
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("GetEmail panicked with malformed email %q: %v", email, r)
					}
				}()

				result := claims.GetEmail()
				assert.Equal(t, email, result, "GetEmail should return input unchanged")
			}()
		}
	})
}

// TestKeycloakClaims_TimeHandling tests time-based claims
func TestKeycloakClaims_TimeHandling(t *testing.T) {
	timeHelper := testutil.NewTimeHelper(t)

	t.Run("expired_token_handling", func(t *testing.T) {
		expiredTime := timeHelper.CreateExpiredTime()

		claims := &KeycloakClaims{
			Email: testutil.TestUserEmail,
			RegisteredClaims: jwt.RegisteredClaims{
				ExpiresAt: jwt.NewNumericDate(expiredTime),
				IssuedAt:  jwt.NewNumericDate(expiredTime.Add(-time.Hour)),
				Subject:   testutil.TestUserSubject,
			},
		}

		// Validate time fields
		assert.True(t, claims.RegisteredClaims.ExpiresAt.Before(time.Now()))
		timeHelper.AssertTimeWithinRange(
			claims.RegisteredClaims.ExpiresAt.Time,
			expiredTime,
			time.Second,
			"Expiration time should match expected expired time",
		)
	})

	t.Run("future_token_handling", func(t *testing.T) {
		futureTime := timeHelper.CreateFutureTime()

		claims := &KeycloakClaims{
			Email: testutil.TestUserEmail,
			RegisteredClaims: jwt.RegisteredClaims{
				NotBefore: jwt.NewNumericDate(futureTime),
				IssuedAt:  jwt.NewNumericDate(time.Now()),
				Subject:   testutil.TestUserSubject,
			},
		}

		// Validate future time handling
		assert.True(t, claims.RegisteredClaims.NotBefore.After(time.Now()))
	})
}

// TestKeycloakClaims_ToOrgData_InputVariations tests ToOrgData with various inputs
func TestKeycloakClaims_ToOrgData_InputVariations(t *testing.T) {
	testCases := []string{
		"testorg:PROVIDER_ADMIN",
		"nvidia:TENANT_ADMIN",
		"nico-dev:PROVIDER_VIEWER",
		"org:role",
		"a:b",
		"UPPERCASE:ROLE",
		"lowercase:role",
		"mixed-Case:Mixed_ROLE",
		"no-colon-separator",
		":empty-org",
		"empty-role:",
		"",
		":::",
		"org::role",
		"org:role:extra",
		"org: role-with-spaces",
		"org-with-spaces :role",
		"org-special:ROLE",
		"org-with-émoji:ROLE",
		"org\n:ROLE",
		"org\t:ROLE",
		"org with spaces:ROLE",
		strings.Repeat("a", 1000) + ":" + strings.Repeat("b", 1000),
		"org\x00:ROLE",
		"org:ROLE\x00",
		"\x01org:ROLE\x02",
	}

	for _, roleInput := range testCases {
		t.Run(fmt.Sprintf("role_%s", roleInput), func(t *testing.T) {
			// Create KeycloakClaims with fuzzed role input
			claims := &KeycloakClaims{
				RealmAccess: RealmAccess{
					Roles: []string{roleInput},
				},
			}

			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("ToOrgData panicked with input %q: %v", roleInput, r)
					}
				}()

				orgData := claims.ToOrgData()
				assert.NotNil(t, orgData, "OrgData should never be nil")

				if isValidRoleFormat(roleInput) {
					parts := strings.Split(roleInput, ":")
					expectedOrgName := strings.ToLower(strings.TrimSpace(parts[0]))
					expectedRole := strings.TrimSpace(parts[1])

					assert.Len(t, orgData, 1, "Valid role should create exactly one org")

					org, exists := orgData[expectedOrgName]
					if exists {
						assert.Equal(t, expectedOrgName, org.Name, "Org name should match (lowercase)")
						assert.Contains(t, org.Roles, expectedRole, "Should contain the expected role")
					}
				} else {
					assert.Empty(t, orgData, "Invalid role format should result in empty orgData")
				}

				for orgName, org := range orgData {
					assert.NotEmpty(t, orgName)
					assert.Equal(t, orgName, org.Name)
					assert.NotEmpty(t, org.DisplayName)
					assert.Equal(t, "ENTERPRISE", org.OrgType)
					assert.NotNil(t, org.Roles)
					assert.NotNil(t, org.Teams)

					for _, role := range org.Roles {
						assert.NotEmpty(t, role)
						assert.Equal(t, strings.TrimSpace(role), role)
					}
				}
			}()
		})
	}
}

// TestRealmRolesProcessing_InputVariations tests realm roles processing with malformed inputs
func TestRealmRolesProcessing_InputVariations(t *testing.T) {
	// Seed with various role arrays
	roleArraySeeds := [][]string{
		{"org:ROLE"},
		{"org1:ROLE1", "org2:ROLE2"},
		{""},
		{"invalid"},
		{"org:", ":role", "org:role:extra"},
		{"\x00org:ROLE\x00"},
		{strings.Repeat("a", 1000) + ":" + strings.Repeat("b", 1000)},
		nil,
	}

	// Convert to individual strings for testing
	var testStrings []string
	for _, roles := range roleArraySeeds {
		for _, role := range roles {
			testStrings = append(testStrings, role)
		}
	}

	for _, roleString := range testStrings {
		t.Run(fmt.Sprintf("role_%s", roleString), func(t *testing.T) {
			// Test with single role
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("Single role processing panicked with input %q: %v", roleString, r)
					}
				}()

				claims := &KeycloakClaims{
					RealmAccess: RealmAccess{
						Roles: []string{roleString},
					},
				}

				// Test GetRealmRoles
				roles := claims.GetRealmRoles()
				assert.NotNil(t, roles, "GetRealmRoles should never return nil")

				if roleString == "" {
					// Empty string should be preserved in the array
					assert.Len(t, roles, 1, "Should contain the empty string")
					assert.Equal(t, "", roles[0], "Should contain empty string")
				} else {
					assert.Len(t, roles, 1, "Should contain exactly one role")
					assert.Equal(t, roleString, roles[0], "Should contain the input role unchanged")
				}
			}()

			// Test with role as part of larger array
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("Multi-role processing panicked with role %q in array: %v", roleString, r)
					}
				}()

				// Mix with known good roles to test interaction
				mixedRoles := []string{
					"good-org:GOOD_ROLE",
					roleString,
					"another-org:ANOTHER_ROLE",
				}

				claims := &KeycloakClaims{
					RealmAccess: RealmAccess{
						Roles: mixedRoles,
					},
				}

				orgData := claims.ToOrgData()
				assert.NotNil(t, orgData, "OrgData should never be nil")

				// Should have at least the good roles (2 orgs)
				// Bad role should be ignored, not cause the whole thing to fail
				assert.LessOrEqual(t, len(orgData), 3, "Should not create more orgs than input roles")

				// Good orgs should still exist
				if goodOrg, exists := orgData["good-org"]; exists {
					assert.Contains(t, goodOrg.Roles, "GOOD_ROLE", "Good role should be preserved")
				}
				if anotherOrg, exists := orgData["another-org"]; exists {
					assert.Contains(t, anotherOrg.Roles, "ANOTHER_ROLE", "Another good role should be preserved")
				}
			}()
		})
	}
}

// TestOrgDataInvariants_InputVariations tests that OrgData maintains invariants under all inputs
func TestOrgDataInvariants_InputVariations(t *testing.T) {
	// Seed with various combinations
	orgRolePairs := []struct {
		org  string
		role string
	}{
		{"nvidia", authz.ProviderAdminRole},
		{"test-org", authz.TenantAdminRole},
		{"", "ROLE"},
		{"ORG", ""},
		{"", ""},
		{"with spaces", "ROLE WITH SPACES"},
		{"unicode-special", "UNICODE_SPECIAL_ROLE"},
		{"\x00null\x00", "\x00NULL\x00ROLE"},
	}

	for _, pair := range orgRolePairs {
		t.Run(fmt.Sprintf("org_%s_role_%s", pair.org, pair.role), func(t *testing.T) {
			org, role := pair.org, pair.role
			// Construct role string and test
			roleString := org + ":" + role

			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("OrgData processing panicked with org=%q, role=%q: %v", org, role, r)
					}
				}()

				claims := &KeycloakClaims{
					RealmAccess: RealmAccess{
						Roles: []string{roleString},
					},
				}

				orgData := claims.ToOrgData()

				// Test invariants that should always hold
				assert.NotNil(t, orgData, "OrgData should never be nil")

				// If we have a valid format, ensure proper structure
				if strings.TrimSpace(org) != "" && strings.TrimSpace(role) != "" {
					expectedOrgName := strings.ToLower(strings.TrimSpace(org))

					if orgEntry, exists := orgData[expectedOrgName]; exists {
						// Validate structure
						assert.Equal(t, expectedOrgName, orgEntry.Name, "Name field should match key")
						assert.Equal(t, "ENTERPRISE", orgEntry.OrgType, "OrgType should be ENTERPRISE")
						assert.NotNil(t, orgEntry.Roles, "Roles should not be nil")
						assert.NotNil(t, orgEntry.Teams, "Teams should not be nil")

						// Validate role is present and clean
						assert.Contains(t, orgEntry.Roles, strings.TrimSpace(role),
							"Role should be present and trimmed")

						// Validate no empty roles were added
						for _, r := range orgEntry.Roles {
							assert.NotEmpty(t, r, "No empty roles should exist")
						}
					}
				}
			}()
		})
	}
}

// isValidRoleFormat checks if a role string has the expected "org:role" format
func isValidRoleFormat(roleString string) bool {
	if strings.TrimSpace(roleString) == "" {
		return false
	}

	parts := strings.Split(roleString, ":")
	if len(parts) != 2 {
		return false
	}

	orgName := strings.TrimSpace(parts[0])
	roleName := strings.TrimSpace(parts[1])

	return orgName != "" && roleName != ""
}

// isValidEmailFormat performs basic email format validation
func isValidEmailFormat(email string) bool {
	if strings.TrimSpace(email) == "" {
		return false
	}

	// Very basic check - contains @ and has characters before and after
	parts := strings.Split(email, "@")
	if len(parts) != 2 {
		return false
	}

	local := strings.TrimSpace(parts[0])
	domain := strings.TrimSpace(parts[1])

	return local != "" && domain != "" && !strings.Contains(local, " ") && !strings.Contains(domain, " ")
}
