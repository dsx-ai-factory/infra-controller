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

// reset the tables needed for Fabric tests
func testFabricSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Allocation table
	err := dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystem)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*Instance)(nil))
	assert.Nil(t, err)
	// create the Fabric table
	err = dbSession.DB.ResetModel(context.Background(), (*Fabric)(nil))
	assert.Nil(t, err)
}

func TestFabricSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testFabricSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	fbsd := NewFabricDAO(dbSession)
	fb1, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID:                 "IFabricTest1",
		Org:                      ip.Org,
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		Status:                   "Ready",
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb1)

	fbisd := NewFabricDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []FabricCreateInput
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create multiple",
			inputs: []FabricCreateInput{
				{
					FabricID: "IFabricTest2", Org: "testorg", SiteID: site.ID, InfrastructureProviderID: ip.ID, Status: FabricStatusPending,
				},
				{
					FabricID: "IFabricTest3", Org: "testorg", SiteID: site.ID, InfrastructureProviderID: ip.ID, Status: FabricStatusError,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				dbfb, err := fbisd.Create(ctx, nil, input)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, dbfb)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestFabricSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testFabricSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	site2 := TestBuildSite(t, dbSession, ip, "test2", user)
	fbsd := NewFabricDAO(dbSession)
	fb1, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID:                 "IFabricTest1",
		Org:                      ip.Org,
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		Status:                   FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		fbID               string
		siteID             uuid.UUID
		includeRelations   []string
		expectNotNilSite   bool
		expectNotNilIP     bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:               "success without relations",
			fbID:               fb1.ID,
			siteID:             site.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:             "success with relations",
			fbID:             fb1.ID,
			siteID:           site.ID,
			includeRelations: []string{SiteRelationName, InfrastructureProviderRelationName},
			expectError:      false,
			expectNotNilSite: true,
			expectNotNilIP:   true,
		},
		{
			desc:             "error not found when site ID different",
			fbID:             fb1.ID,
			siteID:           site2.ID,
			includeRelations: []string{},
			expectError:      true,
		},
		{
			desc:             "error when not found",
			fbID:             "notexits" + "-" + uuid.New().String(),
			siteID:           site.ID,
			includeRelations: []string{},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := fbsd.GetByID(ctx, nil, tc.fbID, tc.siteID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, got.ID, tc.fbID)
				assert.Equal(t, got.SiteID, tc.siteID)
				if tc.expectNotNilIP {
					assert.NotNil(t, got.InfrastructureProvider)
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

func TestFabricSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testFabricSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	fbsd := NewFabricDAO(dbSession)
	fbs := []*Fabric{}
	for i := 1; i <= 25; i++ {
		fb1, err := fbsd.Create(ctx, nil, FabricCreateInput{
			FabricID:                 fmt.Sprintf("test-%d", i),
			Org:                      ip.Org,
			SiteID:                   site.ID,
			InfrastructureProviderID: ip.ID,
			Status:                   FabricStatusReady,
		})
		fbs = append(fbs, fb1)
		assert.Nil(t, err)
		assert.NotNil(t, fb1)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		filter           FabricFilterInput
		page             paginator.PageInput
		includeRelations []string

		expectCnt           int
		expectTotal         int
		expectFirstFabricID string
		expectNotNilIP      bool
		expectNotNilSite    bool
		expectError         bool
		verifyChildSpanner  bool
	}{
		{
			desc: "getall with Fabric IDs filters but no relations returns objects",
			filter: FabricFilterInput{
				FabricIDs: []string{fbs[0].ID, fbs[1].ID},
			},
			includeRelations:    []string{},
			expectFirstFabricID: fbs[0].ID,
			expectError:         false,
			expectTotal:         2,
			expectCnt:           2,
			verifyChildSpanner:  true,
		},
		{
			desc: "getall with site filters and relations returns objects",
			filter: FabricFilterInput{
				SiteIDs: []uuid.UUID{site.ID},
			},
			page: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			includeRelations:    []string{SiteRelationName},
			expectFirstFabricID: fbs[0].ID,
			expectError:         false,
			expectTotal:         25,
			expectCnt:           20,
			expectNotNilSite:    true,
		},
		{
			desc: "getall with ip filters and relations returns objects",
			filter: FabricFilterInput{
				InfrastructureProviderID: cutil.GetPtr(ip.ID),
			},
			page: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			includeRelations:    []string{InfrastructureProviderRelationName},
			expectFirstFabricID: fbs[0].ID,
			expectError:         false,
			expectTotal:         25,
			expectCnt:           20,
			expectNotNilIP:      true,
		},
		{
			desc:   "getall with offset, limit returns objects",
			filter: FabricFilterInput{},
			page: paginator.PageInput{
				Offset: cutil.GetPtr(10),
				Limit:  cutil.GetPtr(10),
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			includeRelations:    []string{},
			expectFirstFabricID: fbs[10].ID,
			expectError:         false,
			expectTotal:         25,
			expectCnt:           10,
		},
		{
			desc: "getall with search query returns objects",
			filter: FabricFilterInput{
				InfrastructureProviderID: cutil.GetPtr(ip.ID),
				SearchQuery:              cutil.GetPtr("test-10"),
			},
			page: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			includeRelations:    []string{InfrastructureProviderRelationName},
			expectFirstFabricID: fbs[9].ID,
			expectError:         false,
			expectTotal:         1,
			expectCnt:           1,
			expectNotNilIP:      true,
		},
		{
			desc: "case when no objects are returned",
			filter: FabricFilterInput{
				FabricIDs: []string{uuid.New().String()},
			},
			includeRelations: []string{},
			expectError:      false,
			expectTotal:      0,
			expectCnt:        0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := fbsd.GetAll(ctx, nil, tc.filter, tc.page, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			assert.Equal(t, tc.expectCnt, len(objs))
			assert.Equal(t, tc.expectTotal, tot)
			if len(objs) > 0 {
				if tc.expectFirstFabricID != "" {
					assert.Equal(t, tc.expectFirstFabricID, objs[0].ID)
				}

				if tc.expectNotNilIP {
					assert.NotNil(t, objs[0].InfrastructureProvider)
				}

				if tc.expectNotNilSite {
					assert.NotNil(t, objs[0].Site)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestFabricSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testFabricSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ip1 := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test1", "testorg1", user.ID)
	site1 := TestBuildSite(t, dbSession, ip1, "test1", user)
	ip2 := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test2", "testorg2", user.ID)
	site2 := TestBuildSite(t, dbSession, ip2, "test2", user)
	fbsd := NewFabricDAO(dbSession)

	fb1, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID:                 "test-1",
		Org:                      ip1.Org,
		SiteID:                   site1.ID,
		InfrastructureProviderID: ip1.ID,
		Status:                   FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb1)

	fb2, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID:                 "test-2",
		Org:                      ip2.Org,
		SiteID:                   site2.ID,
		InfrastructureProviderID: ip2.ID,
		Status:                   FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		id   string

		paramSiteID uuid.UUID
		paramIpID   *uuid.UUID
		paramStatus *string

		expectedIpID   *uuid.UUID
		expectedSiteID *uuid.UUID
		expectedStatus *string

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:        "can update all fields",
			id:          fb1.ID,
			paramSiteID: site1.ID,
			paramIpID:   cutil.GetPtr(ip2.ID),
			paramStatus: cutil.GetPtr(FabricStatusError),

			expectedSiteID: cutil.GetPtr(site2.ID),
			expectedIpID:   cutil.GetPtr(ip2.ID),
			expectedStatus: cutil.GetPtr(FabricStatusError),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "error when ID not found",
			id:          "asd82",
			paramSiteID: site1.ID,
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := fbsd.Update(ctx, nil, FabricUpdateInput{
				FabricID:                 tc.id,
				SiteID:                   tc.paramSiteID,
				InfrastructureProviderID: tc.paramIpID,
				Status:                   tc.paramStatus,
				IsMissingOnSite:          cutil.GetPtr(true),
			})
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, tc.id, got.ID)
				assert.Equal(t, tc.expectedIpID.String(), got.InfrastructureProviderID.String())
				assert.Equal(t, *tc.expectedStatus, got.Status)
				assert.Equal(t, true, got.IsMissingOnSite)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestFabricSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testFabricSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ip1 := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test1", "testorg1", user.ID)
	site1 := TestBuildSite(t, dbSession, ip1, "test1", user)
	fbsd := NewFabricDAO(dbSession)

	fb1, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID:                 "test-1",
		Org:                      ip1.Org,
		SiteID:                   site1.ID,
		InfrastructureProviderID: ip1.ID,
		Status:                   FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 string
		siteID             uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			id:                 fb1.ID,
			siteID:             site1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			id:            "123stg",
			siteID:        site1.ID,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := fbsd.Delete(ctx, nil, tc.id, tc.siteID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := fbsd.GetByID(ctx, nil, tc.id, tc.siteID, nil)
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

func TestFabricSQLDAO_DeleteAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testFabricSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ip1 := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test1", "testorg1", user.ID)

	site1 := TestBuildSite(t, dbSession, ip1, "test1", user)

	ip2 := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test2", "testorg2", user.ID)

	site2 := TestBuildSite(t, dbSession, ip2, "test2", user)

	fbsd := NewFabricDAO(dbSession)

	fb1, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID:                 "test-1",
		Org:                      ip1.Org,
		SiteID:                   site1.ID,
		InfrastructureProviderID: ip1.ID,
		Status:                   FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb1)

	fb2, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID: "test-2", Org: ip1.Org, SiteID: site1.ID, InfrastructureProviderID: ip1.ID, Status: FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb2)

	fb3, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID: "test-3", Org: ip1.Org, SiteID: site1.ID, InfrastructureProviderID: ip1.ID, Status: FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb3)

	fb4, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID: "test-4", Org: ip1.Org, SiteID: site2.ID, InfrastructureProviderID: ip2.ID, Status: FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb4)

	fb5, err := fbsd.Create(ctx, nil, FabricCreateInput{
		FabricID: "test-5", Org: ip1.Org, SiteID: site2.ID, InfrastructureProviderID: ip2.ID, Status: FabricStatusReady,
	})
	assert.Nil(t, err)
	assert.NotNil(t, fb5)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                   string
		ids                    []string
		siteID                 *uuid.UUID
		expectedError          bool
		expectedRemainingCount int
		verifyChildSpanner     bool
	}{
		{
			desc:                   "can delete by ids",
			ids:                    []string{fb1.ID, fb2.ID},
			siteID:                 cutil.GetPtr(site1.ID),
			expectedError:          false,
			expectedRemainingCount: 1,
			verifyChildSpanner:     true,
		},
		{
			desc:                   "can delete by site id",
			ids:                    nil,
			siteID:                 cutil.GetPtr(site2.ID),
			expectedError:          false,
			expectedRemainingCount: 0,
			verifyChildSpanner:     true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := fbsd.DeleteAll(ctx, nil, tc.ids, tc.siteID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				if tc.ids != nil {
					for _, id := range tc.ids {
						tmp, err := fbsd.GetByID(ctx, nil, id, *tc.siteID, nil)
						assert.NotNil(t, err)
						assert.Nil(t, tmp)
					}
				}
				if tc.siteID != nil {
					_, tcount, err := fbsd.GetAll(ctx, nil, FabricFilterInput{
						SiteIDs: []uuid.UUID{*tc.siteID},
					}, paginator.PageInput{}, nil)
					assert.Nil(t, err)
					assert.Equal(t, tcount, tc.expectedRemainingCount)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
