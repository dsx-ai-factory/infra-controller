// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"
)

func testAllocationConstraintInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for AllocationConstraint tests
func testAllocationConstraintSetupSchema(t *testing.T, dbSession *db.Session) {
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
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create AllocationConstraint table
	err = dbSession.DB.ResetModel(context.Background(), (*AllocationConstraint)(nil))
	assert.Nil(t, err)
}

func testAllocationConstraintBuildInfrastructureProvider(t *testing.T, dbSession *db.Session,
	name string) *InfrastructureProvider {
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

func testAllocationConstraintBuildSite(t *testing.T, dbSession *db.Session,
	ip *InfrastructureProvider, name string) *Site {
	st := &Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
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

func testAllocationConstraintBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testAllocationConstraintBuildUser(t *testing.T, dbSession *db.Session,
	starfleetID string) *User {
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

func testAllocationConstraintBuildAllocation(t *testing.T, dbSession *db.Session,
	ipID uuid.UUID, tenantID uuid.UUID, siteID uuid.UUID, userID uuid.UUID) *Allocation {
	alloc := &Allocation{
		ID:                       uuid.New(),
		Name:                     "test1",
		Description:              cutil.GetPtr("description"),
		InfrastructureProviderID: ipID,
		TenantID:                 tenantID,
		SiteID:                   siteID,
		CreatedBy:                userID,
	}
	_, err := dbSession.DB.NewInsert().Model(alloc).Exec(context.Background())
	assert.Nil(t, err)
	return alloc
}

func testAllocationConstraintBuildInstanceType(t *testing.T, dbSession *db.Session,
	ipID uuid.UUID, siteID uuid.UUID, userID uuid.UUID, name string) *InstanceType {
	insType := &InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		DisplayName:              cutil.GetPtr("display name"),
		Description:              cutil.GetPtr("description"),
		ControllerMachineType:    cutil.GetPtr("controllerMachineType"),
		InfrastructureProviderID: ipID,
		SiteID:                   &siteID,
		Status:                   InstanceTypeStatusPending,
		CreatedBy:                userID,
	}
	_, err := dbSession.DB.NewInsert().Model(insType).Exec(context.Background())
	assert.Nil(t, err)
	return insType
}

func testAllocationConstraintBuildIPBlock(t *testing.T, dbSession *db.Session,
	siteID, infrastructureProviderID *uuid.UUID, name string) *IPBlock {
	ipBlock := &IPBlock{
		ID:                       uuid.New(),
		Name:                     name,
		SiteID:                   *siteID,
		InfrastructureProviderID: *infrastructureProviderID,
		PrefixLength:             8,
	}
	_, err := dbSession.DB.NewInsert().Model(ipBlock).Exec(context.Background())
	assert.Nil(t, err)
	return ipBlock
}

func TestAllocationConstraintSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationConstraintInitDB(t)
	defer dbSession.Close()
	testAllocationConstraintSetupSchema(t, dbSession)
	ip := testAllocationConstraintBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationConstraintBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationConstraintBuildTenant(t, dbSession, "testTenant")
	user := testAllocationConstraintBuildUser(t, dbSession, "testUser")
	insType := testAllocationConstraintBuildInstanceType(t, dbSession,
		ip.ID, site.ID, user.ID, "instance-type-1")
	ipv4Block := testAllocationConstraintBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block")
	alloc1 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)
	alloc2 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)

	asd := NewAllocationConstraintDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []AllocationConstraintCreateInput
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create constraint of InstanceType and IPBlock",
			inputs: []AllocationConstraintCreateInput{
				{
					AllocationID:    alloc1.ID,
					ResourceType:    AllocationResourceTypeInstanceType,
					ResourceTypeID:  insType.ID,
					ConstraintType:  AllocationConstraintTypeReserved,
					ConstraintValue: 100,
					CreatedBy:       user.ID,
				},
				{
					AllocationID:    alloc2.ID,
					ResourceType:    AllocationResourceTypeIPBlock,
					ResourceTypeID:  ipv4Block.ID,
					ConstraintType:  AllocationConstraintTypeReserved,
					ConstraintValue: 100,
					CreatedBy:       user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "failure - foreign key violation on allocation_id",
			inputs: []AllocationConstraintCreateInput{
				{
					AllocationID:    uuid.New(),
					ResourceType:    AllocationResourceTypeIPBlock,
					ResourceTypeID:  ipv4Block.ID,
					ConstraintType:  AllocationConstraintTypeReserved,
					ConstraintValue: 100,
					CreatedBy:       user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "failure - multiple fields with nil",
			inputs: []AllocationConstraintCreateInput{
				{
					AllocationID:    alloc1.ID,
					ResourceType:    "   ",
					ResourceTypeID:  ipv4Block.ID,
					ConstraintType:  AllocationConstraintTypeReserved,
					ConstraintValue: 100,
					CreatedBy:       user.ID,
				},
				{
					AllocationID:    alloc1.ID,
					ResourceType:    AllocationResourceTypeIPBlock,
					ResourceTypeID:  ipv4Block.ID,
					ConstraintType:  " ",
					ConstraintValue: 100,
					CreatedBy:       user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				it, err := asd.Create(ctx, nil, input)
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

func TestAllocationConstraintSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationConstraintInitDB(t)
	defer dbSession.Close()
	testAllocationConstraintSetupSchema(t, dbSession)
	ip := testAllocationConstraintBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationConstraintBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationConstraintBuildTenant(t, dbSession, "testTenant")
	user := testAllocationConstraintBuildUser(t, dbSession, "testUser")
	insType := testAllocationConstraintBuildInstanceType(t, dbSession,
		ip.ID, site.ID, user.ID, "instance-type-1")
	ipv4Block := testAllocationConstraintBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block")
	alloc1 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)
	alloc2 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)

	asd := NewAllocationConstraintDAO(dbSession)

	a1, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
		AllocationID:    alloc1.ID,
		ResourceType:    AllocationResourceTypeInstanceType,
		ResourceTypeID:  insType.ID,
		ConstraintType:  AllocationConstraintTypeReserved,
		ConstraintValue: 10,
		CreatedBy:       user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a1)

	a2, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
		AllocationID:    alloc2.ID,
		ResourceType:    AllocationResourceTypeIPBlock,
		ResourceTypeID:  ipv4Block.ID,
		ConstraintType:  AllocationConstraintTypeReserved,
		ConstraintValue: 10,
		CreatedBy:       user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		a                  *AllocationConstraint
		paramRelations     []string
		expectedError      bool
		expectedErrVal     error
		expectedAllocation bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when AllocationConstraint exists",
			id:                 a1.ID,
			a:                  a1,
			paramRelations:     []string{},
			expectedError:      false,
			expectedAllocation: false,
		},
		{
			desc:               "GetById error when not found",
			id:                 uuid.New(),
			paramRelations:     []string{},
			expectedError:      true,
			expectedErrVal:     db.ErrDoesNotExist,
			expectedAllocation: false,
		},
		{
			desc:               "GetById with Allocation relation",
			id:                 a2.ID,
			a:                  a2,
			paramRelations:     []string{AllocationRelationName},
			expectedError:      false,
			expectedAllocation: false,
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
				if tc.expectedAllocation {
					assert.EqualValues(t, tc.a.AllocationID, got.Allocation.ID)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationConstraintSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationConstraintInitDB(t)
	defer dbSession.Close()
	testAllocationConstraintSetupSchema(t, dbSession)
	ip := testAllocationConstraintBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationConstraintBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationConstraintBuildTenant(t, dbSession, "testTenant")
	user := testAllocationConstraintBuildUser(t, dbSession, "testUser")
	insType := testAllocationConstraintBuildInstanceType(t, dbSession,
		ip.ID, site.ID, user.ID, "instance-type-1")
	ipv4Block := testAllocationConstraintBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block")

	alloc1 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)
	alloc2 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)

	asd := NewAllocationConstraintDAO(dbSession)

	totalCount := 30

	asc1it := []AllocationConstraint{}
	for range totalCount / 2 {
		asc1, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
			AllocationID:    alloc1.ID,
			ResourceType:    AllocationResourceTypeInstanceType,
			ResourceTypeID:  insType.ID,
			ConstraintType:  AllocationConstraintTypeReserved,
			ConstraintValue: 10,
			CreatedBy:       user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, asc1)
		asc1it = append(asc1it, *asc1)
	}

	asc2ipb := []AllocationConstraint{}
	for range totalCount / 2 {
		asc2, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
			AllocationID:      alloc2.ID,
			ResourceType:      AllocationResourceTypeIPBlock,
			ResourceTypeID:    ipv4Block.ID,
			ConstraintType:    AllocationConstraintTypeReserved,
			ConstraintValue:   10,
			DerivedResourceID: cutil.GetPtr(uuid.New()),
			CreatedBy:         user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, asc2)
		asc2ipb = append(asc2ipb, *asc2)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             AllocationConstraintFilterInput
		page               paginator.PageInput
		firstEntry         *AllocationConstraint
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			filter:             AllocationConstraintFilterInput{},
			expectedCount:      paginator.DefaultLimit,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetAll with Allocation ID filter returns objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc2.ID},
			},
			expectedCount:  totalCount / 2,
			expectedError:  false,
			paramRelations: []string{AllocationRelationName},
		},
		{
			desc: "GetAll with Resource Type filter returns objects",
			filter: AllocationConstraintFilterInput{
				ResourceType: cutil.GetPtr(AllocationResourceTypeIPBlock),
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with Resource Type ID filter returns objects",
			filter: AllocationConstraintFilterInput{
				ResourceTypeIDs: []uuid.UUID{insType.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with Derived Resource ID filter returns objects",
			filter: AllocationConstraintFilterInput{
				DerivedResourceID: asc2ipb[0].DerivedResourceID,
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with invalid Resource Type ID filter returns no objects",
			filter: AllocationConstraintFilterInput{
				ResourceTypeIDs: []uuid.UUID{uuid.New()},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with Constraint Type filter returns objects",
			filter: AllocationConstraintFilterInput{
				ResourceType:   cutil.GetPtr(AllocationResourceTypeIPBlock),
				ConstraintType: cutil.GetPtr(AllocationConstraintTypeReserved),
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc1.ID},
			},
			page: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(5),
			},
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc2.ID},
			},
			page: paginator.PageInput{
				Offset: cutil.GetPtr(5),
			},
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc1.ID},
			},
			page: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:    &asc1it[0],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with resource Type filter returns none objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc1.ID},
				ResourceType:  cutil.GetPtr(AllocationResourceTypeIPBlock),
			},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc: "GetAll with resource Type filter returns objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc2.ID},
				ResourceType:  cutil.GetPtr(AllocationResourceTypeIPBlock),
			},
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with resource Type filter with mixed allocation uuids returns objects",
			filter: AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{alloc1.ID, alloc2.ID},
				ResourceType:  cutil.GetPtr(AllocationResourceTypeIPBlock),
			},
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := asd.GetAll(ctx, nil, tc.filter, tc.page, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				if len(tc.paramRelations) > 0 && len(tc.filter.AllocationIDs) > 0 {
					assert.Equal(t, tc.filter.AllocationIDs[0], got[0].Allocation.ID)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationConstraintSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationConstraintInitDB(t)
	defer dbSession.Close()
	testAllocationConstraintSetupSchema(t, dbSession)
	ip := testAllocationConstraintBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationConstraintBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationConstraintBuildTenant(t, dbSession, "testTenant")
	user := testAllocationConstraintBuildUser(t, dbSession, "testUser")
	insType := testAllocationConstraintBuildInstanceType(t, dbSession,
		ip.ID, site.ID, user.ID, "instance-type-1")
	ipv4Block := testAllocationConstraintBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block")
	alloc1 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)
	alloc2 := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)

	asd := NewAllocationConstraintDAO(dbSession)

	derivedResourceID := uuid.New()
	derivedResourceID2 := uuid.New()
	constraintValue := 1
	constraintValue2 := 2
	a1, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
		AllocationID:      alloc1.ID,
		ResourceType:      AllocationResourceTypeInstanceType,
		ResourceTypeID:    insType.ID,
		ConstraintType:    AllocationConstraintTypeReserved,
		ConstraintValue:   constraintValue,
		DerivedResourceID: &derivedResourceID,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc  string
		input AllocationConstraintUpdateInput

		expectedError bool

		expectedAllocationID      *uuid.UUID
		expectedResourceType      *string
		expectedResourceTypeID    *uuid.UUID
		expectedConstraintType    *string
		expectedConstraintValue   *int
		expectedDerivedResourceID *uuid.UUID
		expectedUpdate            bool
		verifyChildSpanner        bool
	}{
		{
			desc: "can update nothing",
			input: AllocationConstraintUpdateInput{
				AllocationConstraintID: a1.ID,
			},

			expectedError:             false,
			expectedAllocationID:      &alloc1.ID,
			expectedResourceType:      cutil.GetPtr(AllocationResourceTypeInstanceType),
			expectedResourceTypeID:    &insType.ID,
			expectedConstraintType:    cutil.GetPtr(AllocationConstraintTypeReserved),
			expectedConstraintValue:   &constraintValue,
			expectedDerivedResourceID: &derivedResourceID,
			expectedUpdate:            false,
			verifyChildSpanner:        true,
		},
		{
			desc: "error updating due to foreign key violation",
			input: AllocationConstraintUpdateInput{
				AllocationConstraintID: a1.ID,
				AllocationID:           &derivedResourceID,
			},

			expectedError:             true,
			expectedAllocationID:      &alloc1.ID,
			expectedResourceType:      cutil.GetPtr(AllocationResourceTypeInstanceType),
			expectedResourceTypeID:    &insType.ID,
			expectedConstraintType:    cutil.GetPtr(AllocationConstraintTypeReserved),
			expectedConstraintValue:   &constraintValue,
			expectedDerivedResourceID: &derivedResourceID,
			expectedUpdate:            true,
		},
		{
			desc: "can update AllocationID ResourceType and ResourceTypeID",
			input: AllocationConstraintUpdateInput{
				AllocationConstraintID: a1.ID,
				AllocationID:           &alloc2.ID,
				ResourceType:           cutil.GetPtr(AllocationResourceTypeIPBlock),
				ResourceTypeID:         &ipv4Block.ID,
			},

			expectedError:             false,
			expectedAllocationID:      &alloc2.ID,
			expectedResourceType:      cutil.GetPtr(AllocationResourceTypeIPBlock),
			expectedResourceTypeID:    &ipv4Block.ID,
			expectedConstraintType:    cutil.GetPtr(AllocationConstraintTypeReserved),
			expectedConstraintValue:   &constraintValue,
			expectedDerivedResourceID: &derivedResourceID,
			expectedUpdate:            true,
		},
		{
			desc: "can update Constraint Type Value and ResourceID",
			input: AllocationConstraintUpdateInput{
				AllocationConstraintID: a1.ID,
				ConstraintType:         cutil.GetPtr(AllocationConstraintTypePreemptible),
				ConstraintValue:        &constraintValue2,
				DerivedResourceID:      &derivedResourceID2,
			},

			expectedError:             false,
			expectedAllocationID:      &alloc2.ID,
			expectedResourceType:      cutil.GetPtr(AllocationResourceTypeIPBlock),
			expectedResourceTypeID:    &ipv4Block.ID,
			expectedConstraintType:    cutil.GetPtr(AllocationConstraintTypePreemptible),
			expectedConstraintValue:   &constraintValue2,
			expectedDerivedResourceID: &derivedResourceID2,
			expectedUpdate:            true,
		},
		{
			desc: "invalid Constraint Type",
			input: AllocationConstraintUpdateInput{
				AllocationConstraintID: a1.ID,
				ConstraintType:         cutil.GetPtr(" "),
			},

			expectedError:             true,
			expectedAllocationID:      &alloc2.ID,
			expectedResourceType:      cutil.GetPtr(AllocationResourceTypeIPBlock),
			expectedResourceTypeID:    &ipv4Block.ID,
			expectedConstraintType:    cutil.GetPtr(AllocationConstraintTypePreemptible),
			expectedConstraintValue:   &constraintValue2,
			expectedDerivedResourceID: &derivedResourceID2,
			expectedUpdate:            true,
		},
		{
			desc: "invalid Resource Type",
			input: AllocationConstraintUpdateInput{
				AllocationConstraintID: a1.ID,
				ResourceType:           cutil.GetPtr(" "),
			},

			expectedError:             true,
			expectedAllocationID:      &alloc2.ID,
			expectedResourceType:      cutil.GetPtr(AllocationResourceTypeIPBlock),
			expectedResourceTypeID:    &ipv4Block.ID,
			expectedConstraintType:    cutil.GetPtr(AllocationConstraintTypePreemptible),
			expectedConstraintValue:   &constraintValue2,
			expectedDerivedResourceID: &derivedResourceID2,
			expectedUpdate:            true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := asd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				assert.NotNil(t, got)
				assert.Equal(t, *tc.expectedAllocationID, got.AllocationID)
				assert.Equal(t, *tc.expectedResourceType, got.ResourceType)
				assert.Equal(t, *tc.expectedResourceTypeID, got.ResourceTypeID)
				assert.Equal(t, *tc.expectedConstraintType, got.ConstraintType)
				assert.Equal(t, *tc.expectedConstraintValue, got.ConstraintValue)
				assert.Equal(t, tc.expectedDerivedResourceID, got.DerivedResourceID)

				if tc.expectedUpdate && got.Updated.String() == a1.Updated.String() {
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

func TestAllocationConstraintSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationConstraintInitDB(t)
	defer dbSession.Close()
	testAllocationConstraintSetupSchema(t, dbSession)
	ip := testAllocationConstraintBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationConstraintBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationConstraintBuildTenant(t, dbSession, "testTenant")
	user := testAllocationConstraintBuildUser(t, dbSession, "testUser")
	insType := testAllocationConstraintBuildInstanceType(t, dbSession,
		ip.ID, site.ID, user.ID, "instance-type-1")
	alloc := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)
	dummyUID := uuid.New()

	asd := NewAllocationConstraintDAO(dbSession)

	a1, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
		AllocationID:      alloc.ID,
		ResourceType:      AllocationResourceTypeInstanceType,
		ResourceTypeID:    insType.ID,
		ConstraintType:    AllocationConstraintTypeReserved,
		ConstraintValue:   10,
		DerivedResourceID: &dummyUID,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, a1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                      string
		input                     AllocationConstraintClearInput
		expectedDerivedResourceID *uuid.UUID
		verifyChildSpanner        bool
	}{
		{
			desc: "can clear derivedResourceId",
			input: AllocationConstraintClearInput{
				AllocationConstraintID: a1.ID,
				DerivedResourceID:      true,
			},
			expectedDerivedResourceID: nil,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := asd.Clear(ctx, nil, tc.input)
			assert.Nil(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.expectedDerivedResourceID == nil,
				got.DerivedResourceID == nil)
			if tc.expectedDerivedResourceID != nil {
				assert.Equal(t, *tc.expectedDerivedResourceID,
					*got.DerivedResourceID)
			}

			assert.True(t, got.Updated.After(a1.Updated))

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationConstraintSQLDAO_DeleteByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testAllocationConstraintInitDB(t)
	defer dbSession.Close()
	testAllocationConstraintSetupSchema(t, dbSession)
	ip := testAllocationConstraintBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testAllocationConstraintBuildSite(t, dbSession, ip, "testSite")
	tenant := testAllocationConstraintBuildTenant(t, dbSession, "testTenant")
	user := testAllocationConstraintBuildUser(t, dbSession, "testUser")
	insType := testAllocationConstraintBuildInstanceType(t, dbSession,
		ip.ID, site.ID, user.ID, "instance-type-1")
	alloc := testAllocationConstraintBuildAllocation(t, dbSession, ip.ID,
		tenant.ID, site.ID, user.ID)

	asd := NewAllocationConstraintDAO(dbSession)

	a1, err := asd.Create(ctx, nil, AllocationConstraintCreateInput{
		AllocationID:    alloc.ID,
		ResourceType:    AllocationResourceTypeInstanceType,
		ResourceTypeID:  insType.ID,
		ConstraintType:  AllocationConstraintTypeReserved,
		ConstraintValue: 10,
		CreatedBy:       user.ID,
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
			desc:               "can delete existing object",
			aID:                a1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			aID:           uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := asd.DeleteByID(ctx, nil, tc.aID)
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
