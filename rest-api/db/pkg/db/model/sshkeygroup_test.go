// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"
)

// reset the tables needed for SSHKeyGroup tests
func testSSHKeyGroupSetupSchema(t *testing.T, dbSession *db.Session) {
	testInstanceSetupSchema(t, dbSession)
	// create the SSHKeyGroup table
	err := dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroup)(nil))
	assert.Nil(t, err)
}

func TestSSHKeyGroupSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skgsd := NewSSHKeyGroupDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		sks                []SSHKeyGroup
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			sks: []SSHKeyGroup{
				{
					Name: "test", Description: cutil.GetPtr("test"), Org: "testorg", TenantID: tenant.ID, Status: SSHKeyGroupStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with null fields",
			sks: []SSHKeyGroup{
				{
					Name: "test", Description: cutil.GetPtr("test"), Org: "testorg", TenantID: tenant.ID, Status: SSHKeyGroupStatusSyncing, CreatedBy: user.ID,
				},
				{
					Name: "test1", Description: nil, Org: "testorg", TenantID: tenant.ID, Version: cutil.GetPtr("1234"), Status: SSHKeyGroupStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "failure - foreign key violation on tenant_id",
			sks: []SSHKeyGroup{
				{
					Name: "test", Description: cutil.GetPtr("test"), Org: "testorg", TenantID: uuid.New(), Status: SSHKeyGroupStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, skg := range tc.sks {
				sk, err := skgsd.Create(
					ctx,
					nil,
					SSHKeyGroupCreateInput{
						Name:        skg.Name,
						Description: skg.Description,
						TenantOrg:   skg.Org,
						TenantID:    skg.TenantID,
						Version:     skg.Version,
						Status:      skg.Status,
						CreatedBy:   skg.CreatedBy,
					},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, sk)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestSSHKeyGroupSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skgsd := NewSSHKeyGroupDAO(dbSession)
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test", cutil.GetPtr("test"), "test", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	assert.NotNil(t, skg1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		sgID               uuid.UUID
		includeRelations   []string
		expectNotNilTenant bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:               "success without relations",
			sgID:               skg1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:               "success with relations",
			sgID:               skg1.ID,
			includeRelations:   []string{"Tenant"},
			expectError:        false,
			expectNotNilTenant: true,
		},
		{
			desc:             "error when not found",
			sgID:             uuid.New(),
			includeRelations: []string{},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skgsd.GetByID(ctx, nil, tc.sgID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilTenant {
					assert.NotNil(t, got.Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skgsd := NewSSHKeyGroupDAO(dbSession)
	var skgs []*SSHKeyGroup
	for i := 1; i <= 25; i++ {
		skg1 := testBuildSSHKeyGroup(t, dbSession, fmt.Sprintf("test-%d", i), cutil.GetPtr("testdesc"), "testorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
		assert.NotNil(t, skg1)
		skgs = append(skgs, skg1)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramNames     []string
		paramTenantIDs []uuid.UUID
		paramIDs       []uuid.UUID
		paramVersions  []string
		paramStatuses  []string
		searchQuery    *string

		paramOffset  *int
		paramLimit   *int
		paramOrderBy *paginator.OrderBy

		expectCnt             int
		expectTotal           int
		expectFirstObjectName string

		expectNotNilTenant bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:                  "getall with name filter but no relations returns objects",
			paramNames:            []string{"test-1"},
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           1,
			expectCnt:             1,
		},
		{
			desc:                  "getall with tenant filters but no relations returns objects",
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with ids filter but no relations returns objects",
			paramIDs:              []uuid.UUID{skgs[0].ID, skgs[1].ID},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           2,
			expectCnt:             2,
		},
		{
			desc:                  "getall with status filters but no relations returns objects",
			paramStatuses:         []string{SSHKeyGroupStatusSyncing},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:             "getall with filters and relations returns objects",
			includeRelations: []string{"Tenant"},
			paramTenantIDs:   []uuid.UUID{tenant.ID},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
			expectNotNilTenant:    true,
		},
		{
			desc:             "getall with offset, limit returns objects",
			includeRelations: []string{},

			paramOffset: cutil.GetPtr(10),
			paramLimit:  cutil.GetPtr(10),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			expectFirstObjectName: "test-11",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             10,
		},
		{
			desc:             "case when no objects are returned",
			includeRelations: []string{},
			expectError:      false,
			paramTenantIDs:   []uuid.UUID{uuid.New()},
			expectTotal:      0,
			expectCnt:        0,
		},
		{
			desc:             "case when no objects are returned",
			includeRelations: []string{},
			expectError:      false,
			paramVersions:    []string{"1245"},
			expectTotal:      0,
			expectCnt:        0,
		},
		{
			desc:                  "getall with name search query returns objects",
			paramTenantIDs:        nil,
			includeRelations:      []string{},
			searchQuery:           cutil.GetPtr("test-"),
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with name search query returns objects",
			paramTenantIDs:        nil,
			includeRelations:      []string{},
			searchQuery:           cutil.GetPtr("test-"),
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with empty search query returns objects",
			paramTenantIDs:        nil,
			includeRelations:      []string{},
			searchQuery:           cutil.GetPtr(""),
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := skgsd.GetAll(
				ctx,
				nil,
				SSHKeyGroupFilterInput{
					Names:          tc.paramNames,
					TenantIDs:      tc.paramTenantIDs,
					SSHKeyGroupIDs: tc.paramIDs,
					Versions:       tc.paramVersions,
					Statuses:       tc.paramStatuses,
					SearchQuery:    tc.searchQuery,
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
				assert.Equal(t, tc.expectFirstObjectName, objs[0].Name)
				if tc.expectNotNilTenant {
					assert.NotNil(t, objs[0].Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGenerateVersionHash(t *testing.T) {
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
	skg := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("testdesc"), tnOrg, tn.ID, cutil.GetPtr("test-version"), SSHKeyGroupStatusSyncing, tnu.ID)

	// Build SSHKeyGroupSiteAssociation
	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site1.ID, cutil.GetPtr("test-version"), SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa1)

	skgsa2 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site2.ID, cutil.GetPtr("test-version"), SSHKeyGroupSiteAssociationStatusDeleting, tnu.ID)
	assert.NotNil(t, skgsa2)

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

	skgsd := NewSSHKeyGroupDAO(dbSession)

	tests := []struct {
		name                string
		tskg                *SSHKeyGroup
		tskgsas             []SSHKeyGroupSiteAssociation
		expectVersionUpdate bool
		expectErr           bool
	}{
		{
			name:                "success case with site and keys",
			tskg:                skg,
			tskgsas:             []SSHKeyGroupSiteAssociation{*skgsa1, *skgsa2},
			expectErr:           false,
			expectVersionUpdate: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			uskgsa, err := skgsd.GenerateAndUpdateVersion(ctx, nil, tc.tskg.ID)
			if tc.expectErr {
				assert.NotNil(t, err)
			} else {
				assert.Nil(t, err)
			}

			if tc.expectVersionUpdate {
				assert.NotNil(t, uskgsa)
				assert.NotEqual(t, *tc.tskg.Version, *uskgsa.Version)

				skgsaDAO := NewSSHKeyGroupSiteAssociationDAO(dbSession)
				for _, tskgsa := range tc.tskgsas {
					uskgsa, err := skgsaDAO.GetByID(ctx, nil, tskgsa.ID, nil)
					assert.Nil(t, err)
					assert.NotEqual(t, *tskgsa.Version, *uskgsa.Version)
					assert.NotEqual(t, tskgsa.Updated, uskgsa.Updated)
				}
			}
		})
	}
}

func TestSSHKeyGroupSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	tenant1 := testOperatingSystemBuildTenant(t, dbSession, "testTenant1")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skgsd := NewSSHKeyGroupDAO(dbSession)
	skg1, err := skgsd.Create(
		ctx,
		nil,
		SSHKeyGroupCreateInput{
			Name:        "test",
			Description: cutil.GetPtr("test"),
			TenantOrg:   "testorg",
			TenantID:    tenant.ID,
			Status:      SSHKeyGroupStatusSyncing,
			CreatedBy:   user.ID,
		},
	)
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		id   uuid.UUID

		paramName     *string
		paramDesc     *string
		paramTenantID *uuid.UUID
		paramVersion  *string
		paramStatus   *string
		paramOrg      *string

		expectedName     *string
		expectedDesc     *string
		expectedTenantID *uuid.UUID
		expectedVersion  *string
		expectedStatus   *string
		expectedOrg      *string

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:          "can update all fields",
			id:            skg1.ID,
			paramName:     cutil.GetPtr("updatedName"),
			paramDesc:     cutil.GetPtr("updatedDesc"),
			paramTenantID: &tenant1.ID,
			paramVersion:  cutil.GetPtr("2341"),
			paramOrg:      cutil.GetPtr("testorg1"),
			paramStatus:   cutil.GetPtr(SSHKeyGroupStatusError),

			expectedName:     cutil.GetPtr("updatedName"),
			expectedDesc:     cutil.GetPtr("updatedDesc"),
			expectedTenantID: &tenant1.ID,
			expectedVersion:  cutil.GetPtr("2341"),
			expectedOrg:      cutil.GetPtr("testorg1"),
			expectedStatus:   cutil.GetPtr(SSHKeyGroupStatusError),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "error when ID not found",
			id:          uuid.New(),
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skgsd.Update(
				ctx,
				nil,
				SSHKeyGroupUpdateInput{
					SSHKeyGroupID: tc.id,
					Name:          tc.paramName,
					Description:   tc.paramDesc,
					TenantOrg:     tc.paramOrg,
					TenantID:      tc.paramTenantID,
					Version:       tc.paramVersion,
					Status:        tc.paramStatus,
				},
			)
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, *tc.expectedName, got.Name)
				assert.Equal(t, tc.expectedDesc, got.Description)
				assert.Equal(t, *tc.expectedOrg, got.Org)
				assert.Equal(t, tc.expectedTenantID.String(), got.TenantID.String())
				assert.Equal(t, *tc.expectedStatus, got.Status)
				assert.Equal(t, tc.expectedVersion, got.Version)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skgsd := NewSSHKeyGroupDAO(dbSession)
	skg1, err := skgsd.Create(
		ctx,
		nil,
		SSHKeyGroupCreateInput{
			Name:        "test",
			Description: cutil.GetPtr("test"),
			TenantOrg:   "testorg",
			TenantID:    tenant.ID,
			Status:      SSHKeyGroupStatusSyncing,
			CreatedBy:   user.ID,
		},
	)
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
			id:                 skg1.ID,
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
			err := skgsd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := skgsd.GetByID(ctx, nil, tc.id, nil)
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
