// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"
	otrace "go.opentelemetry.io/otel/trace"
)

func testAllocationInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for Allocation tests
func testAllocationSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create Instance Type table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create AllocationConstraint table
	err = dbSession.DB.ResetModel(context.Background(), (*AllocationConstraint)(nil))
	assert.Nil(t, err)
}

func testAllocationBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        name,
		DisplayName: cutil.GetPtr("TestInfraProvider"),
		Org:         "test",
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testAllocationBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *Site {
	st := &Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr(name + "-display"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testAllocationBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:             uuid.New(),
		Name:           name,
		OrgDisplayName: cutil.GetPtr(name + "-display"),
		Org:            "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testAllocationBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(starfleetID),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	assert.Nil(t, err)
	return user
}

func TestAllocationSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()
	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationBuildTenant(t, dbSession, "testTenant")
	user := testAllocationBuildUser(t, dbSession, "testUser")
	asd := NewAllocationDAO(dbSession)
	dummyUUID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		as                 []Allocation
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			as: []Allocation{
				{
					Name: "test", InfrastructureProviderID: ip.ID, TenantID: tenant.ID, SiteID: site.ID, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with null description",
			as: []Allocation{
				{
					Name: "test1", Description: cutil.GetPtr("description"), InfrastructureProviderID: ip.ID, TenantID: tenant.ID, SiteID: site.ID, CreatedBy: user.ID,
				},
				{
					Name: "test2", InfrastructureProviderID: ip.ID, TenantID: tenant.ID, SiteID: site.ID, CreatedBy: user.ID,
				},
				{
					Name: "test3", InfrastructureProviderID: ip.ID, TenantID: tenant.ID, SiteID: site.ID, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "failure - foreign key violation on infrastructure_provider_id",
			as: []Allocation{
				{
					Name: "test", InfrastructureProviderID: uuid.New(), TenantID: tenant.ID, SiteID: site.ID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on tenant_id",
			as: []Allocation{
				{
					Name: "test", InfrastructureProviderID: ip.ID, TenantID: dummyUUID, SiteID: site.ID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on site_id",
			as: []Allocation{
				{
					Name: "test", InfrastructureProviderID: ip.ID, TenantID: tenant.ID, SiteID: dummyUUID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.as {

				it, err := asd.Create(ctx, nil, AllocationCreateInput{
					Name:                     i.Name,
					Description:              cutil.GetPtr("description"),
					InfrastructureProviderID: i.InfrastructureProviderID,
					TenantID:                 i.TenantID,
					SiteID:                   i.SiteID,
					Status:                   AllocationStatusPending,
					CreatedBy:                i.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, it)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestAllocationSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()
	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationBuildTenant(t, dbSession, "testTenant")
	user := testAllocationBuildUser(t, dbSession, "testUser")
	asd := NewAllocationDAO(dbSession)
	a, err := asd.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test1",
		Description:              cutil.GetPtr("description"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)
	a2, err := asd.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test2",
		Description:              cutil.GetPtr("description"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                           string
		id                             uuid.UUID
		a                              *Allocation
		paramRelations                 []string
		expectedError                  bool
		expectedErrVal                 error
		expectedInfrastructureProvider bool
		expectedTenant                 bool
		expectedSite                   bool
		verifyChildSpanner             bool
	}{
		{
			desc:                           "GetById success when Allocation exists",
			id:                             a.ID,
			a:                              a,
			paramRelations:                 []string{},
			expectedError:                  false,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
			expectedSite:                   false,
			verifyChildSpanner:             true,
		},
		{
			desc:                           "GetById error when not found",
			id:                             uuid.New(),
			paramRelations:                 []string{},
			expectedError:                  true,
			expectedErrVal:                 db.ErrDoesNotExist,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
			expectedSite:                   false,
		},
		{
			desc:                           "GetById with the infrastructure_provider relation",
			id:                             a.ID,
			a:                              a,
			paramRelations:                 []string{"InfrastructureProvider"},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedTenant:                 false,
			expectedSite:                   false,
		},
		{
			desc:                           "GetById with the infrastructure_provider and Tenant relations",
			id:                             a.ID,
			a:                              a,
			paramRelations:                 []string{"InfrastructureProvider", "Tenant"},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedTenant:                 true,
			expectedSite:                   false,
		},
		{
			desc:                           "GetById with the infrastructure_provider, Tenant and Site relations",
			id:                             a.ID,
			a:                              a,
			paramRelations:                 []string{"InfrastructureProvider", "Tenant", "Site"},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedTenant:                 true,
			expectedSite:                   true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := asd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.a.ID, got.ID)
				if tc.expectedInfrastructureProvider {
					assert.EqualValues(t, tc.a.InfrastructureProviderID, got.InfrastructureProvider.ID)
				}
				if tc.expectedTenant {
					assert.EqualValues(t, tc.a.TenantID, got.Tenant.ID)
				} else {
					assert.Nil(t, got.Tenant)
				}
				if tc.expectedSite {
					assert.EqualValues(t, tc.a.SiteID, got.Site.ID)
				} else {
					assert.Nil(t, got.Site)
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestAllocationSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()

	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testAllocationBuildSite(t, dbSession, ip, "testSite1")
	site2 := testAllocationBuildSite(t, dbSession, ip, "testSite2")
	site3 := testAllocationBuildSite(t, dbSession, ip, "testSite3")
	tenant1 := testAllocationBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testAllocationBuildTenant(t, dbSession, "testTenant2")
	tenant3 := testAllocationBuildTenant(t, dbSession, "testTenant3")
	user := testAllocationBuildUser(t, dbSession, "testUser")

	totalCount := 20

	aDAO := NewAllocationDAO(dbSession)
	acDAO := NewAllocationConstraintDAO(dbSession)

	it := testAllocationConstraintBuildInstanceType(t, dbSession, ip.ID, site1.ID, user.ID, "instance-type-1")
	ipb := testAllocationConstraintBuildIPBlock(t, dbSession, &site1.ID, &ip.ID, "ip-block-1")

	// Create Allocations for Tenant1
	allocationsTenant1 := []Allocation{}
	var allocationConstraint1 *AllocationConstraint
	var allocationConstraint2 *AllocationConstraint
	for i := 0; i < totalCount/2; i++ {
		at, err := aDAO.Create(ctx, nil, AllocationCreateInput{
			Name:                     fmt.Sprintf("test-%v", i),
			Description:              cutil.GetPtr("Test Allocation for Tenant 1"),
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenant1.ID,
			SiteID:                   site1.ID,
			Status:                   AllocationStatusPending,
			CreatedBy:                user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, at)
		allocationsTenant1 = append(allocationsTenant1, *at)
		if i%2 == 0 {
			// Create AllocationConstraint for every other Allocation
			var serr error
			allocationConstraint1, serr = acDAO.Create(ctx, nil, AllocationConstraintCreateInput{
				AllocationID: at.ID, ResourceType: AllocationResourceTypeInstanceType,
				ResourceTypeID: it.ID, ConstraintType: AllocationConstraintTypeReserved,
				ConstraintValue: 5, CreatedBy: user.ID,
			})
			assert.NoError(t, serr)
		} else {
			var serr error
			allocationConstraint2, serr = acDAO.Create(ctx, nil, AllocationConstraintCreateInput{
				AllocationID: at.ID, ResourceType: AllocationResourceTypeIPBlock,
				ResourceTypeID: ipb.ID, ConstraintType: AllocationConstraintTypeReserved,
				ConstraintValue: 10, CreatedBy: user.ID,
			})
			assert.NoError(t, serr)
		}
	}

	// Create Allocations for Tenant2
	for i := 0; i < totalCount/2; i++ {
		at, err := aDAO.Create(ctx, nil, AllocationCreateInput{
			Name:                     fmt.Sprintf("test-%v", i),
			Description:              cutil.GetPtr("Test Allocation for Tenant 2"),
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenant2.ID,
			SiteID:                   site2.ID,
			Status:                   AllocationStatusPending,
			CreatedBy:                user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, at)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                                   string
		infrastructureProvider                 *InfrastructureProvider
		filter                                 AllocationFilterInput
		page                                   paginator.PageInput
		firstEntry                             *Allocation
		expectedCount                          int
		expectedTotal                          *int
		expectedError                          bool
		expectedErrorVal                       error
		includeInfrastructureProviderRelations bool
		includeTenantRelations                 bool
		includeSiteRelations                   bool
		verifyChildSpanner                     bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			expectedCount:      totalCount,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:                   "GetAll with ip filter returns objects",
			filter:                 AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			infrastructureProvider: ip,
			expectedCount:          totalCount,
			expectedError:          false,
		},
		{
			desc:                   "GetAll with ip and name filters returns objects",
			filter:                 AllocationFilterInput{Name: cutil.GetPtr("test-0"), InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			infrastructureProvider: ip,
			expectedCount:          2,
			expectedError:          false,
		},
		{
			desc:                                   "GetAll with include relation returns objects",
			filter:                                 AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			infrastructureProvider:                 ip,
			expectedCount:                          totalCount,
			expectedError:                          false,
			includeInfrastructureProviderRelations: true,
		},
		{
			desc:                                   "GetAll with ip, Tenant filter and relation returns objects",
			filter:                                 AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			expectedCount:                          totalCount / 2,
			expectedError:                          false,
			includeTenantRelations:                 true,
			includeInfrastructureProviderRelations: true,
		},
		{
			desc:                 "GetAll with ip, Site filter and relation returns objects",
			filter:               AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, SiteIDs: []uuid.UUID{site1.ID}},
			expectedCount:        totalCount / 2,
			expectedError:        false,
			includeSiteRelations: true,
		},
		{
			desc:                                   "GetAll with ip, Tenant, and site relation returns objects",
			filter:                                 AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, SiteIDs: []uuid.UUID{site1.ID}},
			expectedCount:                          totalCount / 2,
			expectedError:                          false,
			includeInfrastructureProviderRelations: true,
			includeTenantRelations:                 true,
			includeSiteRelations:                   true,
		},
		{
			desc:          "GetAll with ip filter returns no objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{uuid.New()}},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with Tenant filter returns objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with Tenant and name filters returns objects",
			filter:        AllocationFilterInput{Name: cutil.GetPtr("test-0"), InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with Tenant filter returns no objects",
			filter:        AllocationFilterInput{TenantIDs: []uuid.UUID{tenant3.ID}},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with Site filter returns objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, SiteIDs: []uuid.UUID{site1.ID}},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with Site filter returns no objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, SiteIDs: []uuid.UUID{site3.ID}},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with Resource Type filter returns objects",
			filter: AllocationFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				TenantIDs:                 []uuid.UUID{tenant1.ID},
				SiteIDs:                   []uuid.UUID{site1.ID},
				ResourceTypes:             []string{AllocationResourceTypeInstanceType},
				SearchQuery:               cutil.GetPtr("test-"),
			},
			expectedCount: totalCount / 4,
			expectedError: false,
		},
		{
			desc:          "GetAll with ip, Tenant, and site filters returns objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, SiteIDs: []uuid.UUID{site1.ID}},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with ip and Tenant filters returns no objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant3.ID}, SiteIDs: []uuid.UUID{site3.ID}},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with limit returns objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			page:          paginator.PageInput{Offset: cutil.GetPtr(0), Limit: cutil.GetPtr(5)},
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			page:          paginator.PageInput{Offset: cutil.GetPtr(3)},
			expectedCount: 7,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:   "GetAll with order by returns objects",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: "name",
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[0],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with name search query returns objects",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr("test-")},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with description search query returns objects",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr("Test Allocation for Tenant 1")},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with status search query returns objects",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr(AllocationStatusPending)},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with status search query returns no objects",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr(AllocationStatusDeleting)},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns objects",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr("")},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with AllocationStatusPending status returns objects",
			filter:        AllocationFilterInput{Statuses: []string{AllocationStatusPending}},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with AllocationStatusDeleting status returns no objects",
			filter:        AllocationFilterInput{Statuses: []string{AllocationStatusDeleting}},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by ids returns no objects",
			filter:        AllocationFilterInput{AllocationIDs: []uuid.UUID{uuid.New()}},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by ids returns objects",
			filter:        AllocationFilterInput{AllocationIDs: []uuid.UUID{allocationsTenant1[0].ID, allocationsTenant1[1].ID}},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple tenant IDs filter",
			filter: AllocationFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				TenantIDs:                 []uuid.UUID{tenant1.ID, tenant2.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with multiple status filter",
			filter:        AllocationFilterInput{Statuses: []string{AllocationStatusPending, AllocationStatusDeleting}},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple Resource Type filters",
			filter: AllocationFilterInput{
				ResourceTypes: []string{AllocationResourceTypeInstanceType, AllocationResourceTypeIPBlock},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with single Resource Type ID filter",
			filter: AllocationFilterInput{
				ResourceTypeIDs: []uuid.UUID{allocationConstraint1.ResourceTypeID},
			},
			expectedCount: totalCount / 4,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple Resource Type ID filters",
			filter: AllocationFilterInput{
				ResourceTypeIDs: []uuid.UUID{allocationConstraint1.ResourceTypeID, allocationConstraint2.ResourceTypeID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with single Constraint Type filter",
			filter: AllocationFilterInput{
				ConstraintTypes: []string{allocationConstraint1.ConstraintType},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with single Constraint Value filter",
			filter: AllocationFilterInput{
				ConstraintValues: []int{allocationConstraint1.ConstraintValue},
			},
			expectedCount: totalCount / 4,
			expectedError: false,
		},
		{
			desc:   "GetAll with order by site name no site relation",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderBySiteNameExt,
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[0],
			expectedCount: totalCount,
			expectedTotal: cutil.GetPtr(totalCount),
		},
		{
			desc:   "GetAll with order by site name and site relation",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderBySiteNameExt,
				Order: paginator.OrderAscending,
			}},
			firstEntry:           &allocationsTenant1[0],
			expectedCount:        totalCount,
			expectedTotal:        cutil.GetPtr(totalCount),
			includeSiteRelations: true,
		},
		{
			desc:   "GetAll with order by tenant name no tenant relation",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderByTenantOrgDisplayNameExt,
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[0],
			expectedCount: totalCount,
			expectedTotal: cutil.GetPtr(totalCount),
		},
		{
			desc:   "GetAll with order by tenant name and tenant relation",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderByTenantOrgDisplayNameExt,
				Order: paginator.OrderAscending,
			}},
			firstEntry:             &allocationsTenant1[0],
			expectedCount:          totalCount,
			expectedTotal:          cutil.GetPtr(totalCount),
			includeTenantRelations: true,
		},
		{
			desc:   "GetAll with order by instance type name",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderByInstanceTypeName,
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[0],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
		},
		{
			desc:   "GetAll with order by ip block name",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderByIPBlockName,
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[1],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
		},
		{
			desc:   "GetAll with order by constraint value",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderByConstraintValue,
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[0],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
		},
		{
			desc:   "GetAll with order by instance type name and filter on resource type",
			filter: AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, ResourceTypes: []string{"InstanceType"}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: allocationOrderByInstanceTypeName,
				Order: paginator.OrderAscending,
			}},
			firstEntry:    &allocationsTenant1[0],
			expectedCount: totalCount / 4,
			expectedTotal: cutil.GetPtr(totalCount / 4),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {

			relations := []string{}
			if tc.includeInfrastructureProviderRelations {
				relations = append(relations, InfrastructureProviderRelationName)
			}
			if tc.includeSiteRelations {
				relations = append(relations, SiteRelationName)
			}
			if tc.includeTenantRelations {
				relations = append(relations, TenantRelationName)
			}

			got, total, err := aDAO.GetAll(ctx, nil, tc.filter, tc.page, relations)

			if !tc.expectedError {
				assert.NoError(t, err)
			} else {
				assert.Equal(t, tc.expectedErrorVal, err)
			}

			assert.Equal(t, tc.expectedCount, len(got))

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.includeInfrastructureProviderRelations {
				assert.Equal(t, tc.filter.InfrastructureProviderIDs[0].String(), got[0].InfrastructureProvider.ID.String())
			}
			if tc.includeTenantRelations && len(tc.filter.TenantIDs) > 0 {
				assert.Equal(t, tc.filter.TenantIDs[0].String(), got[0].Tenant.ID.String())
			}
			if tc.includeSiteRelations && len(tc.filter.SiteIDs) > 0 {
				assert.Equal(t, tc.filter.SiteIDs[0].String(), got[0].Site.ID.String())
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.ID, got[0].ID)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()
	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testAllocationBuildSite(t, dbSession, ip, "testSite1")
	site2 := testAllocationBuildSite(t, dbSession, ip, "testSite2")
	ip2 := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP2")
	tenant := testAllocationBuildTenant(t, dbSession, "testTenant")
	tenant2 := testAllocationBuildTenant(t, dbSession, "testTenant2")
	user := testAllocationBuildUser(t, dbSession, "testUser")
	asd := NewAllocationDAO(dbSession)
	a1, err := asd.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test1",
		Description:              cutil.GetPtr("description"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site1.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a1)
	dummyUUID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                             string
		input                            AllocationUpdateInput
		expectedError                    bool
		expectedName                     *string
		expectedDescription              *string
		expectedInfrastructureProviderID *uuid.UUID
		expectedTenantID                 *uuid.UUID
		expectedSiteID                   *uuid.UUID
		expectedStatus                   *string
		verifyChildSpanner               bool
	}{
		{
			desc: "can update string fields, name, description, status",
			input: AllocationUpdateInput{
				AllocationID: a1.ID,
				Name:         cutil.GetPtr("updatedName"),
				Description:  cutil.GetPtr("updatedDescription"),
				Status:       cutil.GetPtr(AllocationStatusRegistered),
			},
			expectedError:                    false,
			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("updatedDescription"),
			expectedInfrastructureProviderID: &ip.ID,
			expectedTenantID:                 &tenant.ID,
			expectedSiteID:                   &site1.ID,
			expectedStatus:                   cutil.GetPtr(AllocationStatusRegistered),
			verifyChildSpanner:               true,
		},
		{
			desc: "can update uuid fields: infrastructureProviderID, tenantID, siteID",
			input: AllocationUpdateInput{
				AllocationID:             a1.ID,
				InfrastructureProviderID: &ip2.ID,
				TenantID:                 &tenant2.ID,
				SiteID:                   &site2.ID,
			},
			expectedError:                    false,
			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("updatedDescription"),
			expectedInfrastructureProviderID: &ip2.ID,
			expectedTenantID:                 &tenant2.ID,
			expectedSiteID:                   &site2.ID,
			expectedStatus:                   cutil.GetPtr(AllocationStatusRegistered),
		},
		{
			desc: "error updating due to foreign key violation",
			input: AllocationUpdateInput{
				AllocationID:             a1.ID,
				InfrastructureProviderID: &dummyUUID,
				TenantID:                 &tenant2.ID,
				SiteID:                   &site1.ID,
			},
			expectedError:                    true,
			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("updatedDescription"),
			expectedInfrastructureProviderID: &ip2.ID,
			expectedTenantID:                 &tenant2.ID,
			expectedSiteID:                   &site2.ID,
			expectedStatus:                   cutil.GetPtr(AllocationStatusRegistered),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := asd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				assert.NotNil(t, got)
				assert.Equal(t, *tc.expectedName, got.Name)
				assert.Equal(t, *tc.expectedDescription, *got.Description)
				assert.Equal(t, *tc.expectedInfrastructureProviderID, got.InfrastructureProviderID)
				assert.Equal(t, *tc.expectedTenantID, got.TenantID)
				assert.Equal(t, *tc.expectedSiteID, got.SiteID)
				assert.Equal(t, *tc.expectedStatus, got.Status)
				if got.Updated.String() == a1.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()
	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testAllocationBuildSite(t, dbSession, ip, "testSite1")
	tenant := testAllocationBuildTenant(t, dbSession, "testTenant2")
	user := testAllocationBuildUser(t, dbSession, "testUser")
	asd := NewAllocationDAO(dbSession)
	a1, err := asd.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test2",
		Description:              cutil.GetPtr("description"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site1.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                string
		a                   *Allocation
		paramDescription    bool
		expectedDescription *string
		verifyChildSpanner  bool
	}{
		{
			desc:                "can clear description",
			a:                   a1,
			paramDescription:    true,
			expectedDescription: nil,
			verifyChildSpanner:  true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := asd.Clear(ctx, nil, AllocationClearInput{AllocationID: tc.a.ID, Description: tc.paramDescription})
			assert.Nil(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.expectedDescription == nil, got.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *got.Description)
			}
			assert.Greater(t, got.Updated, tc.a.Updated)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()
	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testAllocationBuildSite(t, dbSession, ip, "testSite1")
	tenant := testAllocationBuildTenant(t, dbSession, "testTenant2")
	user := testAllocationBuildUser(t, dbSession, "testUser")
	asd := NewAllocationDAO(dbSession)
	a1, err := asd.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test2",
		Description:              cutil.GetPtr("description"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site1.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		aID                uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:          "can delete existing object",
			aID:           a1.ID,
			expectedError: false,
		},
		{
			desc:          "delete non-existing object",
			aID:           uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := asd.Delete(ctx, nil, tc.aID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := asd.GetByID(ctx, nil, tc.aID, nil)
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

func TestAllocationSQLDAO_GetCount(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationInitDB(t)
	defer dbSession.Close()

	testAllocationSetupSchema(t, dbSession)
	ip := testAllocationBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testAllocationBuildSite(t, dbSession, ip, "testSite1")
	site2 := testAllocationBuildSite(t, dbSession, ip, "testSite2")
	tenant1 := testAllocationBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testAllocationBuildTenant(t, dbSession, "testTenant2")
	user := testAllocationBuildUser(t, dbSession, "testUser")

	totalCount := 20

	aDAO := NewAllocationDAO(dbSession)
	acDAO := NewAllocationConstraintDAO(dbSession)

	it := testAllocationConstraintBuildInstanceType(t, dbSession, ip.ID, site1.ID, user.ID, "instance-type-1")
	it2 := testAllocationConstraintBuildInstanceType(t, dbSession, ip.ID, site1.ID, user.ID, "instance-type-2")

	// Create Allocations for Tenant1
	asTenant1 := []Allocation{}
	for i := 0; i < totalCount/2; i++ {
		at, err := aDAO.Create(ctx, nil, AllocationCreateInput{
			Name:                     fmt.Sprintf("test-%v", i),
			Description:              cutil.GetPtr("Test Allocation for Tenant 1"),
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenant1.ID,
			SiteID:                   site1.ID,
			Status:                   AllocationStatusPending,
			CreatedBy:                user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, at)
		asTenant1 = append(asTenant1, *at)
		if i%2 == 0 {
			// Create AllocationConstraint for every other Allocation
			_, serr := acDAO.Create(ctx, nil, AllocationConstraintCreateInput{
				AllocationID: at.ID, ResourceType: AllocationResourceTypeInstanceType,
				ResourceTypeID: it.ID, ConstraintType: AllocationConstraintTypeReserved,
				ConstraintValue: 5, CreatedBy: user.ID,
			})
			assert.NoError(t, serr)
			_, serr = acDAO.Create(ctx, nil, AllocationConstraintCreateInput{
				AllocationID: at.ID, ResourceType: AllocationResourceTypeInstanceType,
				ResourceTypeID: it2.ID, ConstraintType: AllocationConstraintTypeReserved,
				ConstraintValue: 5, CreatedBy: user.ID,
			})
			assert.NoError(t, serr)
		}
	}

	// Create Allocations for Tenant2
	for i := 0; i < totalCount/2; i++ {
		at, err := aDAO.Create(ctx, nil, AllocationCreateInput{
			Name:                     fmt.Sprintf("test-%v", i),
			Description:              cutil.GetPtr("Test Allocation for Tenant 2"),
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenant2.ID,
			SiteID:                   site2.ID,
			Status:                   AllocationStatusPending,
			CreatedBy:                user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, at)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc          string
		filter        AllocationFilterInput
		expectedCount int
	}{
		{
			desc:          "GetCount with no filter",
			expectedCount: totalCount,
		},
		{
			desc:          "GetCount with ip filter",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			expectedCount: totalCount,
		},
		{
			desc:          "GetCount with ip and name filter",
			filter:        AllocationFilterInput{Name: cutil.GetPtr("test-0"), InfrastructureProviderIDs: []uuid.UUID{ip.ID}},
			expectedCount: 2,
		},
		{
			desc:          "GetCount with ip, Tenant filter",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			expectedCount: totalCount / 2,
		},
		{
			desc:          "GetCount with ip, Site filter",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, SiteIDs: []uuid.UUID{site1.ID}},
			expectedCount: totalCount / 2,
		},
		{
			desc:          "GetCount with ip, Tenant, and site",
			filter:        AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, SiteIDs: []uuid.UUID{site1.ID}},
			expectedCount: totalCount / 2,
		},
		{
			desc:          "GetCount with Tenant and name filters",
			filter:        AllocationFilterInput{Name: cutil.GetPtr("test-0"), InfrastructureProviderIDs: []uuid.UUID{ip.ID}, TenantIDs: []uuid.UUID{tenant1.ID}},
			expectedCount: 1,
		},
		{
			desc: "GetCount with Resource Type filter",
			filter: AllocationFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				TenantIDs:                 []uuid.UUID{tenant1.ID},
				SiteIDs:                   []uuid.UUID{site1.ID},
				ResourceTypes:             []string{AllocationResourceTypeInstanceType},
				SearchQuery:               cutil.GetPtr("test-"),
			},
			expectedCount: totalCount / 4,
		},
		{
			desc:          "GetCount with name search query",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr("test-")},
			expectedCount: totalCount,
		},
		{
			desc:          "GetCount with description search query",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr("Test Allocation for Tenant 1")},
			expectedCount: totalCount,
		},
		{
			desc:          "GetCount with status search query",
			filter:        AllocationFilterInput{SearchQuery: cutil.GetPtr(AllocationStatusPending)},
			expectedCount: totalCount,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			count, err := aDAO.GetCount(ctx, nil, tc.filter)
			if err != nil {
				t.Logf("%s", err.Error())
			}
			assert.Equal(t, tc.expectedCount, count)
		})
	}
}
