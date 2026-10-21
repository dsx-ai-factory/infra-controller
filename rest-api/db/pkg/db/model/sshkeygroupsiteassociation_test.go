// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestSSHKeyGroupSiteAssociation_ToKeysetIdentifierProto(t *testing.T) {
	groupID := uuid.New()
	skgsa := &SSHKeyGroupSiteAssociation{
		SSHKeyGroupID: groupID,
		SSHKeyGroup:   &SSHKeyGroup{Org: "org-1"},
	}
	got := skgsa.ToKeysetIdentifierProto()
	require.NotNil(t, got)
	assert.Equal(t, groupID.String(), got.KeysetId)
	assert.Equal(t, "org-1", got.OrganizationId)
}

func TestSSHKeyGroupSiteAssociation_ToKeysetIdentifierProto_NoSSHKeyGroup(t *testing.T) {
	groupID := uuid.New()
	skgsa := &SSHKeyGroupSiteAssociation{SSHKeyGroupID: groupID}
	got := skgsa.ToKeysetIdentifierProto()
	require.NotNil(t, got)
	assert.Equal(t, groupID.String(), got.KeysetId)
	assert.Equal(t, "", got.OrganizationId)
}

func TestSSHKeyGroupSiteAssociation_ToProto(t *testing.T) {
	groupID := uuid.New()

	t.Run("populates identifier, content, and version", func(t *testing.T) {
		version := "v1"
		skgsa := &SSHKeyGroupSiteAssociation{
			SSHKeyGroupID: groupID,
			SSHKeyGroup:   &SSHKeyGroup{Org: "org-1"},
			Version:       &version,
		}
		content := &corev1.TenantKeysetContent{
			PublicKeys: []*corev1.TenantPublicKey{{PublicKey: "ssh-rsa abc"}},
		}
		got := skgsa.ToProto(content)
		require.NotNil(t, got)
		require.NotNil(t, got.KeysetIdentifier)
		assert.Equal(t, groupID.String(), got.KeysetIdentifier.KeysetId)
		assert.Equal(t, "org-1", got.KeysetIdentifier.OrganizationId)
		assert.Equal(t, content, got.KeysetContent)
		assert.Equal(t, "v1", got.Version)
	})

	t.Run("nil version yields empty wire version", func(t *testing.T) {
		skgsa := &SSHKeyGroupSiteAssociation{
			SSHKeyGroupID: groupID,
			SSHKeyGroup:   &SSHKeyGroup{Org: "org-1"},
		}
		got := skgsa.ToProto(nil)
		require.NotNil(t, got)
		assert.Equal(t, "", got.Version)
		assert.Nil(t, got.KeysetContent)
	})
}

func TestSSHKeyGroupSiteAssociation_FromProto(t *testing.T) {
	groupID := uuid.New()

	t.Run("nil proto leaves receiver unchanged", func(t *testing.T) {
		version := "preserved"
		skgsa := &SSHKeyGroupSiteAssociation{SSHKeyGroupID: groupID, Version: &version}
		skgsa.FromProto(nil)
		assert.Equal(t, groupID, skgsa.SSHKeyGroupID)
		require.NotNil(t, skgsa.Version)
		assert.Equal(t, "preserved", *skgsa.Version)
	})

	t.Run("invalid keyset id leaves SSHKeyGroupID unchanged", func(t *testing.T) {
		skgsa := &SSHKeyGroupSiteAssociation{SSHKeyGroupID: groupID}
		skgsa.FromProto(&corev1.TenantKeyset{
			KeysetIdentifier: &corev1.TenantKeysetIdentifier{KeysetId: "not-a-uuid"},
			Version:          "v1",
		})
		assert.Equal(t, groupID, skgsa.SSHKeyGroupID)
		require.NotNil(t, skgsa.Version)
		assert.Equal(t, "v1", *skgsa.Version)
	})

	t.Run("populates SSHKeyGroupID and Version from proto", func(t *testing.T) {
		newID := uuid.New()
		skgsa := &SSHKeyGroupSiteAssociation{}
		skgsa.FromProto(&corev1.TenantKeyset{
			KeysetIdentifier: &corev1.TenantKeysetIdentifier{KeysetId: newID.String()},
			Version:          "v2",
		})
		assert.Equal(t, newID, skgsa.SSHKeyGroupID)
		require.NotNil(t, skgsa.Version)
		assert.Equal(t, "v2", *skgsa.Version)
	})

	t.Run("empty proto version clears Version", func(t *testing.T) {
		stale := "stale"
		skgsa := &SSHKeyGroupSiteAssociation{Version: &stale}
		skgsa.FromProto(&corev1.TenantKeyset{
			KeysetIdentifier: &corev1.TenantKeysetIdentifier{KeysetId: groupID.String()},
			Version:          "",
		})
		assert.Nil(t, skgsa.Version)
	})

	t.Run("nil KeysetIdentifier leaves SSHKeyGroupID unchanged", func(t *testing.T) {
		skgsa := &SSHKeyGroupSiteAssociation{SSHKeyGroupID: groupID}
		skgsa.FromProto(&corev1.TenantKeyset{Version: "v1"})
		assert.Equal(t, groupID, skgsa.SSHKeyGroupID)
		require.NotNil(t, skgsa.Version)
		assert.Equal(t, "v1", *skgsa.Version)
	})
}

func TestSSHKeyGroupSiteAssociation_ToCreateRequestProto(t *testing.T) {
	groupID := uuid.New()
	version := "v1"
	skgsa := &SSHKeyGroupSiteAssociation{
		SSHKeyGroupID: groupID,
		SSHKeyGroup:   &SSHKeyGroup{Org: "org-1"},
		Version:       &version,
	}
	content := &corev1.TenantKeysetContent{}
	got := skgsa.ToCreateRequestProto(content)
	require.NotNil(t, got)
	require.NotNil(t, got.KeysetIdentifier)
	assert.Equal(t, groupID.String(), got.KeysetIdentifier.KeysetId)
	assert.Equal(t, "v1", got.Version)
	assert.Equal(t, content, got.KeysetContent)
}

func TestSSHKeyGroupSiteAssociation_ToUpdateRequestProto(t *testing.T) {
	groupID := uuid.New()
	version := "v2"
	skgsa := &SSHKeyGroupSiteAssociation{
		SSHKeyGroupID: groupID,
		SSHKeyGroup:   &SSHKeyGroup{Org: "org-1"},
		Version:       &version,
	}
	content := &corev1.TenantKeysetContent{}
	got := skgsa.ToUpdateRequestProto(content)
	require.NotNil(t, got)
	require.NotNil(t, got.KeysetIdentifier)
	assert.Equal(t, groupID.String(), got.KeysetIdentifier.KeysetId)
	assert.Equal(t, "org-1", got.KeysetIdentifier.OrganizationId)
	assert.Equal(t, "v2", got.Version)
	assert.Equal(t, content, got.KeysetContent)
}

func TestSSHKeyGroupSiteAssociation_ToDeletionRequestProto(t *testing.T) {
	groupID := uuid.New()
	skgsa := &SSHKeyGroupSiteAssociation{
		SSHKeyGroupID: groupID,
		SSHKeyGroup:   &SSHKeyGroup{Org: "org-1"},
	}
	got := skgsa.ToDeletionRequestProto()
	require.NotNil(t, got)
	require.NotNil(t, got.KeysetIdentifier)
	assert.Equal(t, groupID.String(), got.KeysetIdentifier.KeysetId)
	assert.Equal(t, "org-1", got.KeysetIdentifier.OrganizationId)
}

// reset the tables needed for SSHKeyGroupSiteAssociation tests
func testSSHKeyGroupSiteAssociationSetupSchema(t *testing.T, dbSession *db.Session) {
	testSSHKeyGroupSetupSchema(t, dbSession)
	// create the SSHKeyGroupSiteAssociation table
	err := dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)
}

func TestSSHKeyGroupSiteAssociationSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSiteAssociationSetupSchema(t, dbSession)

	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	sshKeyGroup2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("test2"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	skgsd := NewSSHKeyGroupSiteAssociationDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		skgas              []SSHKeyGroupSiteAssociation
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			skgas: []SSHKeyGroupSiteAssociation{
				{
					SSHKeyGroupID: sshKeyGroup1.ID, SiteID: site.ID, Version: cutil.GetPtr("1224"), Status: SSHKeyGroupSiteAssociationStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple",
			skgas: []SSHKeyGroupSiteAssociation{
				{
					SSHKeyGroupID: sshKeyGroup1.ID, SiteID: site.ID, Status: SSHKeyGroupSiteAssociationStatusSyncing, CreatedBy: user.ID,
				},
				{
					SSHKeyGroupID: sshKeyGroup2.ID, SiteID: site.ID, Status: SSHKeyGroupSiteAssociationStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, skg := range tc.skgas {
				skga, err := skgsd.Create(ctx, nil, SSHKeyGroupSiteAssociationCreateInput{
					SSHKeyGroupID: skg.SSHKeyGroupID,
					SiteID:        skg.SiteID,
					Version:       skg.Version,
					Status:        skg.Status,
					CreatedBy:     skg.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, skga)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestSSHKeyGroupSiteAssociationSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	skasd := NewSSHKeyGroupSiteAssociationDAO(dbSession)
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	skga1, err := skasd.Create(ctx, nil, SSHKeyGroupSiteAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup1.ID,
		SiteID:        site.ID,
		Status:        SSHKeyGroupSiteAssociationStatusSyncing,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                    string
		skgaID                  uuid.UUID
		includeRelations        []string
		expectNotNilSSHKeyGroup bool
		expectNotNilSite        bool
		expectError             bool
		verifyChildSpanner      bool
	}{
		{
			desc:               "success without relations",
			skgaID:             skga1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:                    "success with relations",
			skgaID:                  skga1.ID,
			includeRelations:        []string{SSHKeyGroupRelationName, SiteRelationName},
			expectError:             false,
			expectNotNilSSHKeyGroup: true,
		},
		{
			desc:             "error when not found",
			skgaID:           uuid.New(),
			includeRelations: []string{},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skasd.GetByID(ctx, nil, tc.skgaID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilSSHKeyGroup {
					assert.NotNil(t, got.SSHKeyGroup)
				}
				if tc.expectNotNilSite {
					assert.NotNil(t, got.Site)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupSiteAssociationSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	skgs := []*SSHKeyGroup{}
	skgsd := NewSSHKeyGroupDAO(dbSession)
	skgasd := NewSSHKeyGroupSiteAssociationDAO(dbSession)
	for i := 1; i <= 25; i++ {
		skg1, err := skgsd.Create(
			ctx,
			nil,
			SSHKeyGroupCreateInput{
				Name:        fmt.Sprintf("test-%d", i),
				Description: cutil.GetPtr(fmt.Sprintf("test-%d", i)),
				TenantOrg:   "testorg",
				TenantID:    tenant.ID,
				Status:      SSHKeyGroupStatusSyncing,
				CreatedBy:   user.ID,
			},
		)
		skgs = append(skgs, skg1)
		assert.Nil(t, err)
		assert.NotNil(t, skg1)
		skga1, err := skgasd.Create(ctx, nil, SSHKeyGroupSiteAssociationCreateInput{
			SSHKeyGroupID: skg1.ID,
			SiteID:        site.ID,
			Status:        SSHKeyGroupSiteAssociationStatusSyncing,
			CreatedBy:     user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, skga1)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramSSHKeyGroupIDs []uuid.UUID
		paramSiteID         *uuid.UUID
		paramVersion        *string
		paramStatus         *string

		paramOffset  *int
		paramLimit   *int
		paramOrderBy *paginator.OrderBy

		expectCnt                      int
		expectTotal                    int
		expectFirstObjectSSHKeyGroupID string

		expectNotNilSSHKeyGroup bool
		expectError             bool
		verifyChildSpanner      bool
	}{
		{
			desc:                           "getall with SSHKeyGroup filters but no relations returns objects",
			paramSSHKeyGroupIDs:            []uuid.UUID{skgs[0].ID, skgs[1].ID},
			includeRelations:               []string{},
			expectFirstObjectSSHKeyGroupID: skgs[0].ID.String(),
			expectError:                    false,
			expectTotal:                    2,
			expectCnt:                      2,
			verifyChildSpanner:             true,
		},
		{
			desc:             "getall with filters and relations returns objects",
			includeRelations: []string{SSHKeyGroupRelationName},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupID: skgs[0].ID.String(),
			expectError:                    false,
			expectTotal:                    25,
			expectCnt:                      20,
			expectNotNilSSHKeyGroup:        true,
		},
		{
			desc:             "getall with status filters and relations returns objects",
			includeRelations: []string{SSHKeyGroupRelationName},
			paramStatus:      cutil.GetPtr(SSHKeyGroupSiteAssociationStatusSyncing),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupID: skgs[0].ID.String(),
			expectError:                    false,
			expectTotal:                    25,
			expectCnt:                      20,
			expectNotNilSSHKeyGroup:        true,
		},
		{
			desc:             "getall with site filters and relations returns objects",
			includeRelations: []string{SSHKeyGroupRelationName},
			paramSiteID:      cutil.GetPtr(site.ID),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupID: skgs[0].ID.String(),
			expectError:                    false,
			expectTotal:                    25,
			expectCnt:                      20,
			expectNotNilSSHKeyGroup:        true,
		},
		{
			desc:             "getall with offset, limit returns objects",
			includeRelations: []string{},
			paramOffset:      cutil.GetPtr(10),
			paramLimit:       cutil.GetPtr(10),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupID: skgs[10].ID.String(),
			expectError:                    false,
			expectTotal:                    25,
			expectCnt:                      10,
		},
		{
			desc:                "case when no objects are returned",
			includeRelations:    []string{},
			expectError:         false,
			paramSSHKeyGroupIDs: []uuid.UUID{uuid.New()},
			expectTotal:         0,
			expectCnt:           0,
		},
		{
			desc:             "case when filter by controller keyset version no objects are returned",
			includeRelations: []string{},
			expectError:      false,
			paramVersion:     cutil.GetPtr("1234"),
			expectTotal:      0,
			expectCnt:        0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := skgasd.GetAll(
				ctx,
				nil,
				SSHKeyGroupSiteAssociationFilterInput{
					SSHKeyGroupIDs: tc.paramSSHKeyGroupIDs,
					SiteID:         tc.paramSiteID,
					Version:        tc.paramVersion,
					Status:         tc.paramStatus,
				},
				paginator.PageInput{
					Offset:  tc.paramOffset,
					Limit:   tc.paramLimit,
					OrderBy: tc.paramOrderBy,
				},
				tc.includeRelations,
			)
			assert.Equal(t, tc.expectError, err != nil)
			assert.Equal(t, tc.expectCnt, len(objs))
			assert.Equal(t, tc.expectTotal, tot)
			if len(objs) > 0 {
				if tc.expectFirstObjectSSHKeyGroupID != "" {
					assert.Equal(t, tc.expectFirstObjectSSHKeyGroupID, objs[0].SSHKeyGroupID.String())
				}

				if tc.expectNotNilSSHKeyGroup {
					assert.NotNil(t, objs[0].SSHKeyGroup)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupSiteAssociationSQLDAO_GenerateAndUpdateVersion(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	tnOrg := "test-tenant-org-1"

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", ipOrg1, ipu.ID)
	assert.NotNil(t, ip)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe1@test.com"), cutil.GetPtr("John1"), cutil.GetPtr("Doe2"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	assert.NotNil(t, tn)

	site1 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-1", "Test Site-1", ip.Org, ipu.ID)
	site2 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-2", "Test Site-2", ip.Org, ipu.ID)

	// Build SSHKeyGroup
	skg := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("testdesc"), tnOrg, tn.ID, nil, SSHKeyGroupStatusSyncing, tnu.ID)

	// Build SSHKeyGroupSiteAssociation
	skga1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site1.ID, cutil.GetPtr("test-version"), SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skga1)

	skga2 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site2.ID, cutil.GetPtr("test-version"), SSHKeyGroupSiteAssociationStatusDeleting, tnu.ID)
	assert.NotNil(t, skga2)

	// Build SSHKey
	sk1 := testBuildSSHKey(t, dbSession, "test-ssh-key-1", tnOrg, tn.ID, "testpublickey", cutil.GetPtr("test"), nil, tn.ID)
	assert.NotNil(t, sk1)

	sk2 := testBuildSSHKey(t, dbSession, "test-ssh-key-2", tnOrg, tn.ID, "testpublickey", cutil.GetPtr("test"), nil, tn.ID)
	assert.NotNil(t, sk2)

	// Build SSHKeyAssociation
	ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg.ID, tn.ID)
	assert.NotNil(t, ska1)

	ska2 := testBuildSSHKeyAssociation(t, dbSession, sk2.ID, skg.ID, tn.ID)
	assert.NotNil(t, ska2)

	skgad := NewSSHKeyGroupSiteAssociationDAO(dbSession)

	tests := []struct {
		name          string
		skga          *SSHKeyGroupSiteAssociation
		expectVersion bool
		expectErr     bool
	}{
		{
			name:          "success case with keys",
			skga:          skga1,
			expectErr:     false,
			expectVersion: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			uskga, err := skgad.GenerateAndUpdateVersion(ctx, nil, tc.skga.ID)

			if tc.expectErr {
				assert.NotNil(t, err)
			} else {
				assert.Nil(t, err)
			}

			if tc.expectVersion {
				assert.NotNil(t, uskga)
				assert.NotEqual(t, *tc.skga.Version, *uskga.Version)
			}
		})
	}
}

func TestSSHKeyGroupSiteAssociationSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	site2 := TestBuildSite(t, dbSession, ip, "test2", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	skgasd := NewSSHKeyGroupSiteAssociationDAO(dbSession)
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	sshKeyGroup2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("test2"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	skga1, err := skgasd.Create(ctx, nil, SSHKeyGroupSiteAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup1.ID,
		SiteID:        site.ID,
		Status:        SSHKeyGroupSiteAssociationStatusSyncing,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc  string
		input SSHKeyGroupSiteAssociationUpdateInput

		expectedSSHKeyGroupID   *uuid.UUID
		expectedSiteID          *uuid.UUID
		expectedVersion         *string
		expectedStatus          *string
		expectedIsMissingOnSite *bool

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "can update all fields",
			input: SSHKeyGroupSiteAssociationUpdateInput{
				ID:              skga1.ID,
				SSHKeyGroupID:   cutil.GetPtr(sshKeyGroup2.ID),
				SiteID:          cutil.GetPtr(site2.ID),
				Version:         cutil.GetPtr("1234"),
				Status:          cutil.GetPtr(SSHKeyGroupSiteAssociationStatusError),
				IsMissingOnSite: cutil.GetPtr(true),
			},

			expectedSSHKeyGroupID:   cutil.GetPtr(sshKeyGroup2.ID),
			expectedVersion:         cutil.GetPtr("1234"),
			expectedSiteID:          cutil.GetPtr(site2.ID),
			expectedStatus:          cutil.GetPtr(SSHKeyGroupSiteAssociationStatusError),
			expectedIsMissingOnSite: cutil.GetPtr(true),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "error when ID not found",
			input:       SSHKeyGroupSiteAssociationUpdateInput{ID: uuid.New()},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skgasd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, tc.expectedSSHKeyGroupID.String(), got.SSHKeyGroupID.String())
				assert.Equal(t, tc.expectedVersion, got.Version)
				assert.Equal(t, tc.expectedSiteID.String(), got.SiteID.String())
				assert.Equal(t, *tc.expectedStatus, got.Status)
				assert.Equal(t, *tc.expectedIsMissingOnSite, got.IsMissingOnSite)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupSiteAssociationSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	skgasd := NewSSHKeyGroupSiteAssociationDAO(dbSession)
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	skga1, err := skgasd.Create(ctx, nil, SSHKeyGroupSiteAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup1.ID,
		SiteID:        site.ID,
		Status:        SSHKeyGroupSiteAssociationStatusSyncing,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			id:                 skga1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			id:            uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := skgasd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := skgasd.GetByID(ctx, nil, tc.id, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
