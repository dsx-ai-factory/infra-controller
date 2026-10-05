// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authz

import (
	"context"
	"testing"

	"github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/core/claim"
	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestValidateOrgMembership(t *testing.T) {
	// Set up DB
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	org := "test-org"
	orgRole := "test-role"

	// Add user entry
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: cdbm.OrgData{
			org: cdbm.Org{
				ID:      123,
				Name:    org,
				OrgType: "ENTERPRISE",
				Roles:   []string{orgRole},
			},
		},
	}

	_, err = dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	require.NoError(t, err)

	type args struct {
		user *cdbm.User
		org  string
	}

	tests := []struct {
		name    string
		args    args
		want    bool
		wantErr bool
	}{
		{
			name: "valid org membership",
			args: args{
				user: user,
				org:  org,
			},
			want:    true,
			wantErr: false,
		},
		{
			name: "invalid org membership",
			args: args{
				user: user,
				org:  "invalid-org",
			},
			want:    false,
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ValidateOrgMembership(tt.args.user, tt.args.org)
			if (err != nil) != tt.wantErr {
				t.Errorf("ValidateOrgMembership() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if got != tt.want {
				t.Errorf("ValidateOrgMembership() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestValidateNgcUserRoles(t *testing.T) {
	// Set up DB
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	providerOrg := "test-provider-org"
	tenantOrg := "test-tenant-org"
	tenantTeam := "test-tenant-team"
	providerRole := ProviderAdminRole
	tenantRole := TenantAdminRole

	// Add user entry
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: cdbm.OrgData{
			providerOrg: cdbm.Org{
				ID:      123,
				Name:    providerOrg,
				OrgType: "ENTERPRISE",
				Roles:   []string{providerRole},
			},
			tenantOrg: cdbm.Org{
				ID:      456,
				Name:    tenantOrg,
				OrgType: "ENTERPRISE",
				Teams: []cdbm.Team{
					{
						ID:    789,
						Name:  tenantTeam,
						Roles: []string{tenantRole},
					},
				},
			},
		},
	}

	_, err = dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	require.NoError(t, err)

	type args struct {
		orgName     string
		ngcTeamName *string
		targetRoles []string
	}

	tests := []struct {
		name string
		args args
		want bool
	}{
		{
			name: "valid provider org role returns true",
			args: args{
				orgName:     providerOrg,
				ngcTeamName: nil,
				targetRoles: []string{providerRole},
			},
			want: true,
		},
		{
			name: "non-existent provider org role returns false",
			args: args{
				orgName:     tenantOrg,
				ngcTeamName: nil,
				targetRoles: []string{providerRole},
			},
			want: false,
		},
		{
			name: "valid tenant team role returns true",
			args: args{
				orgName:     tenantOrg,
				ngcTeamName: cutil.GetPtr(tenantTeam),
				targetRoles: []string{tenantRole},
			},
			want: true,
		},
		{
			name: "non-existent tenant team role returns false",
			args: args{
				orgName:     providerOrg,
				ngcTeamName: nil,
				targetRoles: []string{tenantRole},
			},
			want: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			orgDetails := user.OrgData[tt.args.orgName]

			if got := ValidateUserRolesInOrg(orgDetails, tt.args.ngcTeamName, tt.args.targetRoles...); got != tt.want {
				t.Errorf("ValidateUserRolesInOrg() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestValidateOrgMembershipCaseInsensitive(t *testing.T) {
	// Test case-insensitive org membership validation for both NGC and Keycloak data formats

	// NGC-style data with mixed case org names
	ngcUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("ngc@test.com"),
		FirstName:   cutil.GetPtr("NGC"),
		LastName:    cutil.GetPtr("User"),
		OrgData: cdbm.OrgData{
			"nvidia": cdbm.Org{
				ID:          1,
				Name:        "nvidia",
				DisplayName: "nvidia",
				OrgType:     "ENTERPRISE",
				Roles:       []string{ProviderAdminRole},
				Teams:       []cdbm.Team{},
			},
			"fh93zk6uqtt1": cdbm.Org{
				ID:          38732,
				Name:        "fh93zk6uqtt1",
				DisplayName: "NICo-Tenant-Dev",
				OrgType:     "ENTERPRISE",
				Roles:       []string{TenantAdminRole},
				Teams:       []cdbm.Team{},
			},
		},
	}

	// Keycloak-style data with lowercase org names
	keycloakUser := &cdbm.User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("keycloak@test.com"),
		FirstName:   cutil.GetPtr("Keycloak"),
		LastName:    cutil.GetPtr("User"),
		OrgData: cdbm.OrgData{
			"nico-tenant-dev": cdbm.Org{
				ID:          0,
				Name:        "nico-tenant-dev",
				DisplayName: "nico-tenant-dev",
				OrgType:     "ENTERPRISE",
				Roles:       []string{TenantAdminRole},
				Teams:       []cdbm.Team{},
			},
			"nico-prime-provider": cdbm.Org{
				ID:          0,
				Name:        "nico-prime-provider",
				DisplayName: "nico-prime-provider",
				OrgType:     "ENTERPRISE",
				Roles:       []string{ProviderAdminRole},
				Teams:       []cdbm.Team{},
			},
		},
	}

	tests := []struct {
		name     string
		user     *cdbm.User
		orgName  string
		expected bool
		wantErr  bool
	}{
		// NGC data tests
		{
			name:     "NGC - exact case match",
			user:     ngcUser,
			orgName:  "nvidia",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "NGC - uppercase input",
			user:     ngcUser,
			orgName:  "NVIDIA",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "NGC - mixed case input",
			user:     ngcUser,
			orgName:  "NVidia",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "NGC - complex org name exact match",
			user:     ngcUser,
			orgName:  "fh93zk6uqtt1",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "NGC - complex org name uppercase",
			user:     ngcUser,
			orgName:  "FH93ZK6UQTT1",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "NGC - non-existent org",
			user:     ngcUser,
			orgName:  "nonexistent",
			expected: false,
			wantErr:  false,
		},
		// Keycloak data tests
		{
			name:     "Keycloak - exact case match",
			user:     keycloakUser,
			orgName:  "nico-tenant-dev",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "Keycloak - uppercase input",
			user:     keycloakUser,
			orgName:  "NICO-TENANT-DEV",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "Keycloak - mixed case input",
			user:     keycloakUser,
			orgName:  "NICo-Tenant-Dev",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "Keycloak - provider org exact match",
			user:     keycloakUser,
			orgName:  "nico-prime-provider",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "Keycloak - provider org uppercase",
			user:     keycloakUser,
			orgName:  "NICO-PRIME-PROVIDER",
			expected: true,
			wantErr:  false,
		},
		{
			name:     "Keycloak - non-existent org",
			user:     keycloakUser,
			orgName:  "nonexistent",
			expected: false,
			wantErr:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ValidateOrgMembership(tt.user, tt.orgName)
			if (err != nil) != tt.wantErr {
				t.Errorf("ValidateOrgMembership() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if got != tt.expected {
				t.Errorf("ValidateOrgMembership() = %v, want %v", got, tt.expected)
			}
		})
	}
}

func TestValidateUserRolesCaseInsensitive(t *testing.T) {
	// Test case-insensitive user role validation for both NGC and Keycloak data formats

	// NGC-style data with mixed case org names
	ngcUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("ngc@test.com"),
		FirstName:   cutil.GetPtr("NGC"),
		LastName:    cutil.GetPtr("User"),
		OrgData: cdbm.OrgData{
			"nvidia": cdbm.Org{
				ID:          1,
				Name:        "nvidia",
				DisplayName: "nvidia",
				OrgType:     "ENTERPRISE",
				Roles:       []string{ProviderAdminRole, TenantAdminRole},
				Teams: []cdbm.Team{
					{
						ID:       44225,
						Name:     "qa1",
						TeamType: "",
						Roles:    []string{ProviderAdminRole, TenantAdminRole},
					},
				},
			},
		},
	}

	// Keycloak-style data with lowercase org names
	keycloakUser := &cdbm.User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("keycloak@test.com"),
		FirstName:   cutil.GetPtr("Keycloak"),
		LastName:    cutil.GetPtr("User"),
		OrgData: cdbm.OrgData{
			"nico-tenant-dev": cdbm.Org{
				ID:          0,
				Name:        "nico-tenant-dev",
				DisplayName: "nico-tenant-dev",
				OrgType:     "ENTERPRISE",
				Roles:       []string{TenantAdminRole},
				Teams:       []cdbm.Team{},
			},
		},
	}

	tests := []struct {
		name        string
		user        *cdbm.User
		orgName     string
		teamName    *string
		targetRoles []string
		expected    bool
	}{
		// NGC data tests - org level roles
		{
			name:        "NGC - exact case org role match",
			user:        ngcUser,
			orgName:     "nvidia",
			teamName:    nil,
			targetRoles: []string{ProviderAdminRole},
			expected:    true,
		},
		{
			name:        "NGC - uppercase org name role match",
			user:        ngcUser,
			orgName:     "NVIDIA",
			teamName:    nil,
			targetRoles: []string{ProviderAdminRole},
			expected:    true,
		},
		{
			name:        "NGC - mixed case org name role match",
			user:        ngcUser,
			orgName:     "NVidia",
			teamName:    nil,
			targetRoles: []string{TenantAdminRole},
			expected:    true,
		},
		// NGC data tests - team level roles
		{
			name:        "NGC - exact case team role match",
			user:        ngcUser,
			orgName:     "nvidia",
			teamName:    cutil.GetPtr("qa1"),
			targetRoles: []string{ProviderAdminRole},
			expected:    true,
		},
		{
			name:        "NGC - uppercase org name team role match",
			user:        ngcUser,
			orgName:     "NVIDIA",
			teamName:    cutil.GetPtr("qa1"),
			targetRoles: []string{TenantAdminRole},
			expected:    true,
		},
		// Keycloak data tests
		{
			name:        "Keycloak - exact case org role match",
			user:        keycloakUser,
			orgName:     "nico-tenant-dev",
			teamName:    nil,
			targetRoles: []string{TenantAdminRole},
			expected:    true,
		},
		{
			name:        "Keycloak - uppercase org name role match",
			user:        keycloakUser,
			orgName:     "NICO-TENANT-DEV",
			teamName:    nil,
			targetRoles: []string{TenantAdminRole},
			expected:    true,
		},
		{
			name:        "Keycloak - mixed case org name role match",
			user:        keycloakUser,
			orgName:     "NICo-Tenant-Dev",
			teamName:    nil,
			targetRoles: []string{TenantAdminRole},
			expected:    true,
		},
		// Negative tests
		{
			name:        "NGC - non-existent org",
			user:        ngcUser,
			orgName:     "nonexistent",
			teamName:    nil,
			targetRoles: []string{ProviderAdminRole},
			expected:    false,
		},
		{
			name:        "Keycloak - non-existent org",
			user:        keycloakUser,
			orgName:     "nonexistent",
			teamName:    nil,
			targetRoles: []string{TenantAdminRole},
			expected:    false,
		},
		{
			name:        "NGC - wrong role",
			user:        ngcUser,
			orgName:     "nvidia",
			teamName:    nil,
			targetRoles: []string{"NONEXISTENT_ROLE"},
			expected:    false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ValidateUserRoles(tt.user, tt.orgName, tt.teamName, tt.targetRoles...)
			if got != tt.expected {
				t.Errorf("ValidateUserRoles() = %v, want %v", got, tt.expected)
			}
		})
	}
}

func TestKeycloakRealmAccessToOrgDataValidation(t *testing.T) {
	// Test the complete flow: Keycloak realmAccess -> orgData extraction -> org role validation

	// Set up DB
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	tests := []struct {
		name                string
		realmAccessRoles    []string // Raw Keycloak realm_access.roles
		testOrgName         string   // Org to test membership for
		testTeamName        *string  // Team to test (nil for org-level)
		targetRoles         []string // Roles to validate
		expectOrgMembership bool     // Should user be member of testOrgName?
		expectRoleMatch     bool     // Should user have the target roles?
		description         string   // Test description
	}{
		{
			name: "Keycloak user with single org admin role",
			realmAccessRoles: []string{
				"nico-tenant-dev:TENANT_ADMIN",
			},
			testOrgName:         "nico-tenant-dev",
			testTeamName:        nil,
			targetRoles:         []string{TenantAdminRole},
			expectOrgMembership: true,
			expectRoleMatch:     true,
			description:         "User has TENANT_ADMIN role for nico-tenant-dev org",
		},
		{
			name: "Keycloak user with multiple org roles",
			realmAccessRoles: []string{
				"nico-tenant-dev:TENANT_ADMIN",
				"nico-prime-provider:PROVIDER_ADMIN",
				"nico-prime-provider:PROVIDER_VIEWER",
			},
			testOrgName:         "nico-prime-provider",
			testTeamName:        nil,
			targetRoles:         []string{ProviderAdminRole},
			expectOrgMembership: true,
			expectRoleMatch:     true,
			description:         "User has multiple roles in nico-prime-provider org",
		},
		{
			name: "Keycloak user testing different org",
			realmAccessRoles: []string{
				"nico-tenant-dev:TENANT_ADMIN",
				"nico-prime-provider:PROVIDER_ADMIN",
			},
			testOrgName:         "nico-tenant-dev",
			testTeamName:        nil,
			targetRoles:         []string{TenantAdminRole},
			expectOrgMembership: true,
			expectRoleMatch:     true,
			description:         "User has TENANT_ADMIN role for nico-tenant-dev org",
		},
		{
			name: "Keycloak user with wrong role for org",
			realmAccessRoles: []string{
				"nico-tenant-dev:TENANT_ADMIN",
				"nico-prime-provider:PROVIDER_VIEWER",
			},
			testOrgName:         "nico-prime-provider",
			testTeamName:        nil,
			targetRoles:         []string{ProviderAdminRole}, // User only has VIEWER, not ADMIN
			expectOrgMembership: true,
			expectRoleMatch:     false,
			description:         "User is member but doesn't have the required ADMIN role",
		},
		{
			name: "Keycloak user not member of tested org",
			realmAccessRoles: []string{
				"nico-tenant-dev:TENANT_ADMIN",
				"other-org:PROVIDER_ADMIN",
			},
			testOrgName:         "nonexistent-org",
			testTeamName:        nil,
			targetRoles:         []string{TenantAdminRole},
			expectOrgMembership: false,
			expectRoleMatch:     false,
			description:         "User is not a member of the tested org",
		},
		{
			name: "Keycloak user with case-insensitive org matching",
			realmAccessRoles: []string{
				"NICO-TENANT-DEV:TENANT_ADMIN", // Uppercase in realmAccess
			},
			testOrgName:         "nico-tenant-dev", // Lowercase in test
			testTeamName:        nil,
			targetRoles:         []string{TenantAdminRole},
			expectOrgMembership: true,
			expectRoleMatch:     true,
			description:         "Case-insensitive org name matching should work",
		},
		{
			name: "Keycloak user with multiple roles in same org",
			realmAccessRoles: []string{
				"nvidia:PROVIDER_ADMIN",
				"nvidia:TENANT_ADMIN",
				"nvidia:PROVIDER_VIEWER",
			},
			testOrgName:         "nvidia",
			testTeamName:        nil,
			targetRoles:         []string{ProviderViewerRole}, // Test for viewer role
			expectOrgMembership: true,
			expectRoleMatch:     true,
			description:         "User with multiple roles in same org should match any target role",
		},
		{
			name: "Keycloak user with no valid realm roles",
			realmAccessRoles: []string{
				"invalid-format-role", // No colon separator
				"",                    // Empty role
			},
			testOrgName:         "any-org",
			testTeamName:        nil,
			targetRoles:         []string{TenantAdminRole},
			expectOrgMembership: false,
			expectRoleMatch:     false,
			description:         "Invalid realm role formats should not create org memberships",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Step 1: Create Keycloak claims with realmAccess roles
			keycloakClaims := &claim.KeycloakClaims{
				Email:     "keycloak-user@test.com",
				FirstName: "Keycloak",
				LastName:  "User",
				RealmAccess: claim.RealmAccess{
					Roles: tt.realmAccessRoles,
				},
			}

			// Step 2: Extract orgData from Keycloak claims (this is what happens in the auth flow)
			orgData := keycloakClaims.ToOrgData()

			// Step 3: Create user with the extracted orgData
			user := &cdbm.User{
				ID:          uuid.New(),
				AuxiliaryID: cutil.GetPtr(uuid.New().String()), // Keycloak users use AuxiliaryID
				Email:       cutil.GetPtr(keycloakClaims.Email),
				FirstName:   cutil.GetPtr(keycloakClaims.FirstName),
				LastName:    cutil.GetPtr(keycloakClaims.LastName),
				OrgData:     orgData, // This is the extracted orgData from realmAccess
			}

			// Step 4: Insert user into database
			_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
			if err != nil {
				t.Fatalf("Failed to insert test user: %v", err)
			}

			// Step 5: Test org membership validation
			gotMembership, err := ValidateOrgMembership(user, tt.testOrgName)
			if err != nil {
				t.Errorf("ValidateOrgMembership() error = %v", err)
				return
			}
			if gotMembership != tt.expectOrgMembership {
				t.Errorf("ValidateOrgMembership() = %v, want %v. %s", gotMembership, tt.expectOrgMembership, tt.description)
			}

			// Step 6: Test role validation (only if user is expected to be a member)
			if tt.expectOrgMembership {
				gotRoles := ValidateUserRoles(user, tt.testOrgName, tt.testTeamName, tt.targetRoles...)
				if gotRoles != tt.expectRoleMatch {
					t.Errorf("ValidateUserRoles() = %v, want %v. %s", gotRoles, tt.expectRoleMatch, tt.description)
				}
			}

			// Step 7: Debug output for failed tests
			if t.Failed() {
				t.Logf("Debug info for failed test:")
				t.Logf("  Original realmAccess.Roles: %v", tt.realmAccessRoles)
				t.Logf("  Extracted orgData: %+v", orgData)
				t.Logf("  Testing org: %s", tt.testOrgName)
				t.Logf("  Target roles: %v", tt.targetRoles)
				if orgDetails, err := user.OrgData.GetOrgByName(tt.testOrgName); err == nil {
					t.Logf("  User's roles in org: %v", orgDetails.Roles)
				}
			}
		})
	}
}

func TestKeycloakRealmAccessEdgeCases(t *testing.T) {
	// Test edge cases in Keycloak realmAccess processing

	tests := []struct {
		name             string
		realmAccessRoles []string
		expectedOrgCount int
		description      string
	}{
		{
			name:             "Empty realmAccess roles",
			realmAccessRoles: []string{},
			expectedOrgCount: 0,
			description:      "Empty roles should result in no org memberships",
		},
		{
			name: "Mixed valid and invalid role formats",
			realmAccessRoles: []string{
				"valid-org:TENANT_ADMIN",       // Valid
				"invalid-format",               // Invalid - no colon (ignored)
				"another-valid:PROVIDER_ADMIN", // Valid
				":EMPTY_ORG",                   // Invalid - empty org name (skipped)
				"EMPTY_ROLE:",                  // Invalid - empty role (skipped)
			},
			expectedOrgCount: 2, // Only valid roles with non-empty org and role names
			description:      "Only valid role formats with non-empty parts create org memberships",
		},
		{
			name: "Duplicate roles for same org",
			realmAccessRoles: []string{
				"test-org:TENANT_ADMIN",
				"test-org:TENANT_ADMIN",   // Duplicate (will be deduplicated)
				"test-org:PROVIDER_ADMIN", // Different role, same org
			},
			expectedOrgCount: 1, // Only 1 org, with deduplicated roles
			description:      "Duplicate roles are deduplicated by ToOrgData()",
		},
		{
			name: "Case variations in org names",
			realmAccessRoles: []string{
				"Test-Org:TENANT_ADMIN",
				"TEST-ORG:PROVIDER_ADMIN",  // Same org, different case
				"test-org:PROVIDER_VIEWER", // Same org, different case
			},
			expectedOrgCount: 1, // All should map to same org (lowercase)
			description:      "Different case variations should map to same org",
		},
		{
			name: "Whitespace trimming in org names and roles",
			realmAccessRoles: []string{
				" test-org :TENANT_ADMIN",      // Spaces around org name
				"test-org: PROVIDER_ADMIN ",    // Spaces around role
				" test-org : PROVIDER_VIEWER ", // Spaces around both
			},
			expectedOrgCount: 1, // All should map to same org after trimming
			description:      "Whitespace should be trimmed from org names and roles",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create Keycloak claims
			keycloakClaims := &claim.KeycloakClaims{
				RealmAccess: claim.RealmAccess{
					Roles: tt.realmAccessRoles,
				},
			}

			// Extract orgData
			orgData := keycloakClaims.ToOrgData()

			// Validate expected org count
			if len(orgData) != tt.expectedOrgCount {
				t.Errorf("Expected %d orgs, got %d. %s", tt.expectedOrgCount, len(orgData), tt.description)
				t.Logf("  Input roles: %v", tt.realmAccessRoles)
				t.Logf("  Resulting orgData: %+v", orgData)
			}

			// Additional validation for specific test cases
			if tt.name == "Duplicate roles for same org" && len(orgData) == 1 {
				for _, org := range orgData {
					// Should have 2 unique roles (duplicate removed)
					if len(org.Roles) != 2 {
						t.Errorf("Expected 2 unique roles (duplicate removed), got %d: %v", len(org.Roles), org.Roles)
					}
					// Should contain only one instance of TENANT_ADMIN
					adminCount := 0
					for _, role := range org.Roles {
						if role == TenantAdminRole {
							adminCount++
						}
					}
					if adminCount != 1 {
						t.Errorf("Expected 1 instance of TENANT_ADMIN (deduplicated), got %d", adminCount)
					}
					// Should also contain PROVIDER_ADMIN
					providerCount := 0
					for _, role := range org.Roles {
						if role == ProviderAdminRole {
							providerCount++
						}
					}
					if providerCount != 1 {
						t.Errorf("Expected 1 instance of PROVIDER_ADMIN, got %d", providerCount)
					}
				}
			}

			if tt.name == "Case variations in org names" && len(orgData) == 1 {
				for orgName, org := range orgData {
					// Org name should be lowercase
					if orgName != "test-org" {
						t.Errorf("Expected org name to be 'test-org', got '%s'", orgName)
					}
					// Should have 3 different roles
					if len(org.Roles) != 3 {
						t.Errorf("Expected 3 roles, got %d: %v", len(org.Roles), org.Roles)
					}
				}
			}

			if tt.name == "Whitespace trimming in org names and roles" && len(orgData) == 1 {
				for orgName, org := range orgData {
					// Org name should be trimmed and lowercase
					if orgName != "test-org" {
						t.Errorf("Expected org name to be 'test-org' (trimmed), got '%s'", orgName)
					}
					// Should have 3 different roles, all trimmed
					if len(org.Roles) != 3 {
						t.Errorf("Expected 3 roles, got %d: %v", len(org.Roles), org.Roles)
					}
					// Check that roles are properly trimmed
					expectedRoles := map[string]bool{
						TenantAdminRole:    false,
						ProviderAdminRole:  false,
						ProviderViewerRole: false,
					}
					for _, role := range org.Roles {
						if _, exists := expectedRoles[role]; exists {
							expectedRoles[role] = true
						} else {
							t.Errorf("Unexpected role found: '%s'", role)
						}
					}
					for role, found := range expectedRoles {
						if !found {
							t.Errorf("Expected role '%s' not found in roles: %v", role, org.Roles)
						}
					}
				}
			}
		})
	}
}
