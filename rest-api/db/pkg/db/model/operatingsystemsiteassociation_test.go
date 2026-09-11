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

// reset the tables needed for OperatingSystemSiteAssociation tests
func testOperatingSystemSiteAssociationSetupSchema(t *testing.T, dbSession *db.Session) {
	testOperatingSystemSetupSchema(t, dbSession)
	// create the OperatingSystemSiteAssociation table
	err := dbSession.DB.ResetModel(context.Background(), (*OperatingSystemSiteAssociation)(nil))
	assert.Nil(t, err)
}

func TestOperatingSystemSiteAssociationSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSiteAssociationSetupSchema(t, dbSession)

	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	OperatingSystem1 := testBuildImageOperatingSystem(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)
	OperatingSystem2 := testBuildImageOperatingSystem(t, dbSession, "test2", cutil.GetPtr("test2"), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)

	ossd := NewOperatingSystemSiteAssociationDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		osas               []OperatingSystemSiteAssociation
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			osas: []OperatingSystemSiteAssociation{
				{
					OperatingSystemID: OperatingSystem1.ID, SiteID: site.ID, Version: cutil.GetPtr("1224"), Status: OperatingSystemSiteAssociationStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple",
			osas: []OperatingSystemSiteAssociation{
				{
					OperatingSystemID: OperatingSystem1.ID, SiteID: site.ID, Status: OperatingSystemSiteAssociationStatusSyncing, CreatedBy: user.ID,
				},
				{
					OperatingSystemID: OperatingSystem2.ID, SiteID: site.ID, Status: OperatingSystemSiteAssociationStatusSyncing, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, ossr := range tc.osas {
				ossa, err := ossd.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
					OperatingSystemID: ossr.OperatingSystemID,
					SiteID:            ossr.SiteID,
					Version:           ossr.Version,
					Status:            ossr.Status,
					CreatedBy:         ossr.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, ossa)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestOperatingSystemSiteAssociationSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	ossad := NewOperatingSystemSiteAssociationDAO(dbSession)
	OperatingSystem1 := testBuildImageOperatingSystem(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)

	ossa1, err := ossad.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: OperatingSystem1.ID,
		SiteID:            site.ID,
		Status:            OperatingSystemSiteAssociationStatusSyncing,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                        string
		skgaID                      uuid.UUID
		includeRelations            []string
		expectNotNilOperatingSystem bool
		expectNotNilSite            bool
		expectError                 bool
		verifyChildSpanner          bool
	}{
		{
			desc:               "success without relations",
			skgaID:             ossa1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:                        "success with relations",
			skgaID:                      ossa1.ID,
			includeRelations:            []string{OperatingSystemRelationName, SiteRelationName},
			expectError:                 false,
			expectNotNilOperatingSystem: true,
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
			got, err := ossad.GetByID(ctx, nil, tc.skgaID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilOperatingSystem {
					assert.NotNil(t, got.OperatingSystem)
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

func TestOperatingSystemSiteAssociationSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site1 := TestBuildSite(t, dbSession, ip, "test1", user)
	site2 := TestBuildSite(t, dbSession, ip, "test2", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	oss := []*OperatingSystem{}
	osasd := NewOperatingSystemSiteAssociationDAO(dbSession)
	for i := 1; i <= 25; i++ {
		os1 := testBuildImageOperatingSystem(t, dbSession, fmt.Sprintf("test-%d", i), cutil.GetPtr(fmt.Sprintf("test-%d", i)), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)
		oss = append(oss, os1)
		assert.NotNil(t, os1)
		var siteID uuid.UUID
		var status string
		if i%2 == 0 {
			siteID = site1.ID
			status = OperatingSystemStatusReady
		} else {
			siteID = site2.ID
			status = OperatingSystemStatusSyncing
		}
		ossa1, err := osasd.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: os1.ID,
			SiteID:            siteID,
			Status:            status,
			CreatedBy:         user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, ossa1)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramOperatingSystemIDs []uuid.UUID
		paramSiteIDs            []uuid.UUID
		paramVersions           []string
		paramStatuses           []string

		paramOffset  *int
		paramLimit   *int
		paramOrderBy *paginator.OrderBy

		expectCnt                          int
		expectTotal                        int
		expectFirstObjectOperatingSystemID string

		expectNotNilOperatingSystem bool
		expectError                 bool
		verifyChildSpanner          bool
	}{
		{
			desc:                               "getall with OperatingSystem filters but no relations returns objects",
			paramOperatingSystemIDs:            []uuid.UUID{oss[0].ID, oss[1].ID},
			includeRelations:                   []string{},
			expectFirstObjectOperatingSystemID: oss[0].ID.String(),
			expectError:                        false,
			expectTotal:                        2,
			expectCnt:                          2,
			verifyChildSpanner:                 true,
		},
		{
			desc:             "getall with filters and relations returns objects",
			includeRelations: []string{OperatingSystemRelationName},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectOperatingSystemID: oss[0].ID.String(),
			expectError:                        false,
			expectTotal:                        25,
			expectCnt:                          20,
			expectNotNilOperatingSystem:        true,
		},
		{
			desc:             "getall with status filters and relations returns objects",
			includeRelations: []string{OperatingSystemRelationName},
			paramStatuses:    []string{OperatingSystemSiteAssociationStatusSyncing},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectOperatingSystemID: oss[0].ID.String(),
			expectError:                        false,
			expectTotal:                        13,
			expectCnt:                          13,
			expectNotNilOperatingSystem:        true,
		},
		{
			desc:             "getall with multiple status filter values returns objects",
			includeRelations: []string{OperatingSystemRelationName},
			paramStatuses:    []string{OperatingSystemSiteAssociationStatusSyncing, OperatingSystemStatusReady},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectOperatingSystemID: oss[0].ID.String(),
			expectError:                        false,
			expectTotal:                        25,
			expectCnt:                          20,
			expectNotNilOperatingSystem:        true,
		},
		{
			desc:             "getall with site filters and relations returns objects",
			includeRelations: []string{OperatingSystemRelationName},
			paramSiteIDs:     []uuid.UUID{site2.ID},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectOperatingSystemID: oss[0].ID.String(),
			expectError:                        false,
			expectTotal:                        13,
			expectCnt:                          13,
			expectNotNilOperatingSystem:        true,
		},
		{
			desc:             "getall with multiple site filters and relations returns objects",
			includeRelations: []string{OperatingSystemRelationName},
			paramSiteIDs:     []uuid.UUID{site1.ID, site2.ID},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectOperatingSystemID: oss[0].ID.String(),
			expectError:                        false,
			expectTotal:                        25,
			expectCnt:                          20,
			expectNotNilOperatingSystem:        true,
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
			expectFirstObjectOperatingSystemID: oss[10].ID.String(),
			expectError:                        false,
			expectTotal:                        25,
			expectCnt:                          10,
		},
		{
			desc:                    "case when no objects are returned",
			includeRelations:        []string{},
			expectError:             false,
			paramOperatingSystemIDs: []uuid.UUID{uuid.New()},
			expectTotal:             0,
			expectCnt:               0,
		},
		{
			desc:             "case when filter by controller keyset version no objects are returned",
			includeRelations: []string{},
			expectError:      false,
			paramVersions:    []string{"1234"},
			expectTotal:      0,
			expectCnt:        0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := osasd.GetAll(ctx, nil, OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: tc.paramOperatingSystemIDs,
				SiteIDs:            tc.paramSiteIDs,
				Versions:           tc.paramVersions,
				Statuses:           tc.paramStatuses,
			}, paginator.PageInput{
				Limit:   tc.paramLimit,
				Offset:  tc.paramOffset,
				OrderBy: tc.paramOrderBy,
			}, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			assert.Equal(t, tc.expectCnt, len(objs))
			assert.Equal(t, tc.expectTotal, tot)
			if len(objs) > 0 {
				if tc.expectFirstObjectOperatingSystemID != "" {
					assert.Equal(t, tc.expectFirstObjectOperatingSystemID, objs[0].OperatingSystemID.String())
				}

				if tc.expectNotNilOperatingSystem {
					assert.NotNil(t, objs[0].OperatingSystem)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestOperatingSystemSiteAssociationSQLDAO_GenerateAndUpdateVersion(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSiteAssociationSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", ipOrg1, ipu.ID)
	assert.NotNil(t, ip)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe1@test.com"), cutil.GetPtr("John1"), cutil.GetPtr("Doe2"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	assert.NotNil(t, tn)

	site1 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-1", "Test Site-1", ip.Org, ipu.ID)

	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	// Build OperatingSystem
	os := testBuildImageOperatingSystem(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", &ip.ID, &tn.ID, nil, false, OperatingSystemStatusReady, user.ID)

	// Build OperatingSystemSiteAssociation
	ossa1 := testBuildOperatingSystemSiteAssociation(t, dbSession, os.ID, site1.ID, cutil.GetPtr("test-version"), OperatingSystemSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, ossa1)

	skgad := NewOperatingSystemSiteAssociationDAO(dbSession)

	tests := []struct {
		name          string
		ossa          *OperatingSystemSiteAssociation
		expectVersion bool
		expectErr     bool
	}{
		{
			name:          "success case",
			ossa:          ossa1,
			expectErr:     false,
			expectVersion: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			uossa, err := skgad.GenerateAndUpdateVersion(ctx, nil, tc.ossa.ID)

			if tc.expectErr {
				assert.NotNil(t, err)
			} else {
				assert.Nil(t, err)
			}

			if tc.expectVersion {
				assert.NotNil(t, uossa)
				assert.NotEqual(t, *tc.ossa.Version, *uossa.Version)
			}
		})
	}
}

func TestOperatingSystemSiteAssociationSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	site2 := TestBuildSite(t, dbSession, ip, "test2", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	osasd := NewOperatingSystemSiteAssociationDAO(dbSession)

	// Build OperatingSystem
	OperatingSystem1 := testBuildImageOperatingSystem(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)
	OperatingSystem2 := testBuildImageOperatingSystem(t, dbSession, "test2", cutil.GetPtr("test2"), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)

	ossa1, err := osasd.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: OperatingSystem1.ID,
		SiteID:            site.ID,
		Status:            OperatingSystemSiteAssociationStatusSyncing,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		id   uuid.UUID

		paramOperatingSystemID *uuid.UUID
		paramSiteID            *uuid.UUID
		paramVersion           *string
		paramStatus            *string

		expectedOperatingSystemID *uuid.UUID
		expectedSiteID            *uuid.UUID
		expectedVersion           *string
		expectedStatus            *string
		IsMissingOnSite           *bool

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:                   "can update all fields",
			id:                     ossa1.ID,
			paramOperatingSystemID: cutil.GetPtr(OperatingSystem2.ID),
			paramVersion:           cutil.GetPtr("1234"),
			paramSiteID:            cutil.GetPtr(site2.ID),
			paramStatus:            cutil.GetPtr(OperatingSystemSiteAssociationStatusError),

			expectedOperatingSystemID: cutil.GetPtr(OperatingSystem2.ID),
			expectedVersion:           cutil.GetPtr("1234"),
			expectedSiteID:            cutil.GetPtr(site2.ID),
			expectedStatus:            cutil.GetPtr(OperatingSystemSiteAssociationStatusError),
			IsMissingOnSite:           cutil.GetPtr(true),

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
			got, err := osasd.Update(ctx, nil, OperatingSystemSiteAssociationUpdateInput{
				OperatingSystemSiteAssociationID: tc.id,
				OperatingSystemID:                tc.paramOperatingSystemID,
				SiteID:                           tc.paramSiteID,
				Version:                          tc.paramVersion,
				Status:                           tc.paramStatus,
				IsMissingOnSite:                  tc.IsMissingOnSite,
			})
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, tc.expectedOperatingSystemID.String(), got.OperatingSystemID.String())
				assert.Equal(t, tc.expectedVersion, got.Version)
				assert.Equal(t, tc.expectedSiteID.String(), got.SiteID.String())
				assert.Equal(t, *tc.expectedStatus, got.Status)
				assert.Equal(t, *tc.IsMissingOnSite, got.IsMissingOnSite)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestOperatingSystemSiteAssociationSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSiteAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")

	osasd := NewOperatingSystemSiteAssociationDAO(dbSession)
	OperatingSystem1 := testBuildImageOperatingSystem(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", &ip.ID, &tenant.ID, nil, false, OperatingSystemStatusReady, user.ID)

	ossa1, err := osasd.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: OperatingSystem1.ID,
		SiteID:            site.ID,
		Status:            OperatingSystemSiteAssociationStatusSyncing,
		CreatedBy:         user.ID,
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
			id:                 ossa1.ID,
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
			err := osasd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := osasd.GetByID(ctx, nil, tc.id, nil)
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
