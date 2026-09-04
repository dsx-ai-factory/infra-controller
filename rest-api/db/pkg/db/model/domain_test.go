// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"slices"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"
)

func testDomainInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for Domain tests
func testDomainSetupSchema(t *testing.T, dbSession *db.Session) {
	// create user table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*Domain)(nil))
	assert.Nil(t, err)
}

func testDomainBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
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

func TestDomainSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testDomainInitDB(t)
	defer dbSession.Close()
	testDomainSetupSchema(t, dbSession)
	user := testDomainBuildUser(t, dbSession, "testUser")
	controllerDomainID := uuid.New()

	dsd := NewDomainDAO(dbSession)
	tx1, err := db.BeginTx(context.Background(), dbSession, &sql.TxOptions{})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []DomainCreateInput
		expectError        bool
		tx                 *db.Tx
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			inputs: []DomainCreateInput{
				{
					Hostname:           "test.com",
					Org:                "testOrg",
					ControllerDomainID: &controllerDomainID,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
			},
			expectError:        false,
			tx:                 nil,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with null controllerDomainID field",
			inputs: []DomainCreateInput{
				{
					Hostname:           "test1.com",
					Org:                "testOrg1",
					ControllerDomainID: &controllerDomainID,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
				{
					Hostname:           "test2.com",
					Org:                "testOrg2",
					ControllerDomainID: nil,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
				{
					Hostname:           "test3.com",
					Org:                "testOrg3",
					ControllerDomainID: &controllerDomainID,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
			},
			expectError: false,
			tx:          nil,
		},
		{
			desc: "create multiple, within transaction",
			inputs: []DomainCreateInput{
				{
					Hostname:           "test4.com",
					Org:                "testOrg1",
					ControllerDomainID: &controllerDomainID,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
				{
					Hostname:           "test5.com",
					Org:                "testOrg2",
					ControllerDomainID: nil,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
				{
					Hostname:           "test6.com",
					Org:                "testOrg3",
					ControllerDomainID: &controllerDomainID,
					Status:             DomainStatusPending,
					CreatedBy:          user.ID,
				},
			},
			expectError: false,
			tx:          tx1,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				d, err := dsd.Create(ctx, tc.tx, input)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, d)
				}
			}
			if tc.tx != nil {
				require.NoError(t, tc.tx.Commit())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDomainSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testDomainInitDB(t)
	defer dbSession.Close()
	testDomainSetupSchema(t, dbSession)
	user := testDomainBuildUser(t, dbSession, "testUser")
	controllerDomainID := uuid.New()

	dsd := NewDomainDAO(dbSession)
	domain1, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test.com",
		Org:                "testOrg",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		expectedDomain     *Domain
		expectedError      bool
		expectedErrVal     error
		verifyChildSpanner bool
	}{
		{
			desc:               "GetByID success when found",
			id:                 domain1.ID,
			expectedDomain:     domain1,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetByID error when not found",
			id:             uuid.New(),
			expectedDomain: nil,
			expectedError:  true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := dsd.GetByID(ctx, nil, tc.id, nil)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				assert.Equal(t, tc.expectedDomain.ID, got.ID)
				assert.Equal(t, tc.expectedDomain.Hostname, got.Hostname)
			} else {
				assert.Equal(t, tc.expectedErrVal, err)
				assert.Nil(t, got)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDomainSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testDomainInitDB(t)
	defer dbSession.Close()
	testDomainSetupSchema(t, dbSession)
	user := testDomainBuildUser(t, dbSession, "testUser")
	controllerDomainID := uuid.New()

	dsd := NewDomainDAO(dbSession)
	domain1, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test1.com",
		Org:                "testOrg1",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, domain1)
	domain2, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test1.com",
		Org:                "testOrg2",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, domain2)
	domain3, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test2.com",
		Org:                "testOrg2",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, domain3)

	tenantA := uuid.New()
	tenantB := uuid.New()
	siteA := uuid.New()
	siteB := uuid.New()
	ownedControllerDomainID := uuid.New()
	createOwnedDomain := func(hostname string, tenantID, siteID *uuid.UUID) *Domain {
		domain, createErr := dsd.Create(ctx, nil, DomainCreateInput{
			Hostname:           hostname,
			Org:                "ownership-org",
			TenantID:           tenantID,
			SiteID:             siteID,
			ControllerDomainID: &ownedControllerDomainID,
			Status:             DomainStatusReady,
			CreatedBy:          user.ID,
		})
		require.NoError(t, createErr)
		return domain
	}
	tenantASiteA := createOwnedDomain("a-a.example.com", &tenantA, &siteA)
	tenantASiteASecond := createOwnedDomain("a-a.example.com", &tenantA, &siteA)
	createOwnedDomain("a-b.example.com", &tenantA, &siteB)
	createOwnedDomain("b-a.example.com", &tenantB, &siteA)
	legacy := createOwnedDomain("legacy.example.com", nil, nil)

	tiedNameIDs := []string{tenantASiteA.ID.String(), tenantASiteASecond.ID.String()}
	slices.Sort(tiedNameIDs)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             DomainFilterInput
		page               paginator.PageInput
		expectedCnt        int
		expectedTotal      int
		expectedIDs        []string
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			filter:             DomainFilterInput{},
			expectedCnt:        8,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetAll with hostname filter returns objects",
			filter: DomainFilterInput{
				Hostname: cutil.GetPtr("test1.com"),
			},
			expectedCnt:   2,
			expectedError: false,
		},
		{
			desc: "GetAll with org filter returns objects",
			filter: DomainFilterInput{
				Org: cutil.GetPtr("testOrg2"),
			},
			expectedCnt:   2,
			expectedError: false,
		},
		{
			desc: "GetAll with controllerDomainID filter returns objects",
			filter: DomainFilterInput{
				ControllerDomainID: &controllerDomainID,
			},
			expectedCnt:   3,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple filters returns objects",
			filter: DomainFilterInput{
				Hostname:           cutil.GetPtr("test1.com"),
				Org:                cutil.GetPtr("testOrg1"),
				ControllerDomainID: &controllerDomainID,
			},
			expectedCnt:   1,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple filters returns no objects",
			filter: DomainFilterInput{
				Hostname:           cutil.GetPtr("notfound.com"),
				Org:                cutil.GetPtr("testOrg1"),
				ControllerDomainID: &controllerDomainID,
			},
			expectedCnt:   0,
			expectedError: false,
		},
		{
			desc: "GetAll with DomainStatusPending status returns objects",
			filter: DomainFilterInput{
				Status: cutil.GetPtr(DomainStatusPending),
			},
			expectedCnt:   3,
			expectedError: false,
		},
		{
			desc: "GetAll with DomainStatusError status returns no objects",
			filter: DomainFilterInput{
				Status: cutil.GetPtr(DomainStatusError),
			},
			expectedCnt:   0,
			expectedError: false,
		},
		{
			desc:        "tenant ownership filter excludes mixed and legacy rows",
			filter:      DomainFilterInput{TenantIDs: []uuid.UUID{tenantA}},
			expectedCnt: 3,
		},
		{
			desc:        "site ownership filter excludes other sites and legacy rows",
			filter:      DomainFilterInput{SiteIDs: []uuid.UUID{siteA}},
			expectedCnt: 3,
		},
		{
			desc: "Domain ID combines with tenant and Site filters",
			filter: DomainFilterInput{
				DomainIDs: []uuid.UUID{tenantASiteA.ID, legacy.ID},
				TenantIDs: []uuid.UUID{tenantA},
				SiteIDs:   []uuid.UUID{siteA},
			},
			expectedCnt: 1,
			expectedIDs: []string{tenantASiteA.ID.String()},
		},
		{
			desc: "pagination returns total before limit",
			filter: DomainFilterInput{
				ControllerDomainID: &controllerDomainID,
			},
			page:          paginator.PageInput{Offset: cutil.GetPtr(1), Limit: cutil.GetPtr(1)},
			expectedCnt:   1,
			expectedTotal: 3,
		},
		{
			desc: "public name order maps to hostname with stable ID tie break",
			filter: DomainFilterInput{
				DomainIDs: []uuid.UUID{tenantASiteA.ID, tenantASiteASecond.ID},
			},
			page: paginator.PageInput{
				Limit:   cutil.GetPtr(paginator.TotalLimit),
				OrderBy: &paginator.OrderBy{Field: "name", Order: paginator.OrderAscending},
			},
			expectedCnt: 2,
			expectedIDs: tiedNameIDs,
		},
		{
			desc:   "created order is supported",
			filter: DomainFilterInput{DomainIDs: []uuid.UUID{tenantASiteA.ID, tenantASiteASecond.ID}},
			page: paginator.PageInput{
				Limit:   cutil.GetPtr(paginator.TotalLimit),
				OrderBy: &paginator.OrderBy{Field: "created", Order: paginator.OrderDescending},
			},
			expectedCnt: 2,
		},
		{
			desc:   "updated order is supported",
			filter: DomainFilterInput{DomainIDs: []uuid.UUID{tenantASiteA.ID, tenantASiteASecond.ID}},
			page: paginator.PageInput{
				Limit:   cutil.GetPtr(paginator.TotalLimit),
				OrderBy: &paginator.OrderBy{Field: "updated", Order: paginator.OrderAscending},
			},
			expectedCnt: 2,
		},
		{
			desc:          "invalid order field",
			page:          paginator.PageInput{OrderBy: &paginator.OrderBy{Field: "status", Order: paginator.OrderAscending}},
			expectedError: true,
		},
		{
			desc:          "invalid order direction",
			page:          paginator.PageInput{OrderBy: &paginator.OrderBy{Field: "created", Order: "SIDEWAYS"}},
			expectedError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			page := tc.page
			if page.Limit == nil {
				page.Limit = cutil.GetPtr(paginator.TotalLimit)
			}
			tmp, total, err := dsd.GetAll(ctx, nil, tc.filter, page, nil)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Nil(t, tmp)
			} else {
				assert.Equal(t, tc.expectedCnt, len(tmp))
				expectedTotal := tc.expectedCnt
				if tc.expectedTotal != 0 {
					expectedTotal = tc.expectedTotal
				}
				assert.Equal(t, expectedTotal, total)
				if tc.expectedIDs != nil {
					actualIDs := make([]string, 0, len(tmp))
					for _, domain := range tmp {
						actualIDs = append(actualIDs, domain.ID.String())
					}
					assert.Equal(t, tc.expectedIDs, actualIDs)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDomainSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testDomainInitDB(t)
	defer dbSession.Close()
	testDomainSetupSchema(t, dbSession)
	user := testDomainBuildUser(t, dbSession, "testUser")
	controllerDomainID := uuid.New()
	updatedControllerDomainID := uuid.New()
	dsd := NewDomainDAO(dbSession)
	domain, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test.com",
		Org:                "testOrg",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	domain2, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test2.com",
		Org:                "testOrg2",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	tx1, err := db.BeginTx(context.Background(), dbSession, &sql.TxOptions{})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string

		input                      DomainUpdateInput
		paramDomain                *Domain
		expectedError              bool
		expectedHostname           *string
		expectedOrg                *string
		expectedControllerDomainID *uuid.UUID
		expectedStatus             *string
		tx                         *db.Tx
		verifyChildSpanner         bool
	}{
		{
			desc: "can update hostname",
			input: DomainUpdateInput{
				DomainID: domain.ID,
				Hostname: cutil.GetPtr("updated.com"),
			},
			paramDomain:                domain,
			expectedError:              false,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                &domain.Org,
			expectedControllerDomainID: domain.ControllerDomainID,
			expectedStatus:             &domain.Status,
			verifyChildSpanner:         true,
		},
		{
			desc: "error when updating object doesnt exist",
			input: DomainUpdateInput{
				DomainID: uuid.New(),
				Hostname: cutil.GetPtr("updated.com"),
			},
			paramDomain:                domain,
			expectedError:              true,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                &domain.Org,
			expectedControllerDomainID: domain.ControllerDomainID,
			expectedStatus:             &domain.Status,
		},
		{
			desc: "can update org",
			input: DomainUpdateInput{
				DomainID: domain.ID,
				Org:      cutil.GetPtr("updatedOrg"),
			},
			paramDomain:                domain,
			expectedError:              false,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                cutil.GetPtr("updatedOrg"),
			expectedControllerDomainID: domain.ControllerDomainID,
			expectedStatus:             &domain.Status,
		},
		{
			desc: "can update controllerDomainID",
			input: DomainUpdateInput{
				DomainID:           domain.ID,
				ControllerDomainID: &updatedControllerDomainID,
			},
			paramDomain:                domain,
			expectedError:              false,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                cutil.GetPtr("updatedOrg"),
			expectedControllerDomainID: &updatedControllerDomainID,
			expectedStatus:             &domain.Status,
		},
		{
			desc: "can update status",
			input: DomainUpdateInput{
				DomainID: domain.ID,
				Status:   cutil.GetPtr(DomainStatusReady),
			},
			paramDomain:                domain,
			expectedError:              false,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                cutil.GetPtr("updatedOrg"),
			expectedControllerDomainID: &updatedControllerDomainID,
			expectedStatus:             cutil.GetPtr(DomainStatusReady),
		},
		{
			desc: "can update multiple fields",
			input: DomainUpdateInput{
				DomainID:           domain2.ID,
				Hostname:           cutil.GetPtr("updated.com"),
				Org:                cutil.GetPtr("updatedOrg"),
				ControllerDomainID: &updatedControllerDomainID,
				Status:             cutil.GetPtr(DomainStatusReady),
			},
			paramDomain:                domain2,
			expectedError:              false,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                cutil.GetPtr("updatedOrg"),
			expectedControllerDomainID: &updatedControllerDomainID,
			expectedStatus:             cutil.GetPtr(DomainStatusReady),
			tx:                         tx1,
		},
		{
			desc: "noop when no fields are specified",
			input: DomainUpdateInput{
				DomainID: domain2.ID,
			},
			paramDomain:                domain2,
			expectedError:              false,
			expectedHostname:           cutil.GetPtr("updated.com"),
			expectedOrg:                cutil.GetPtr("updatedOrg"),
			expectedControllerDomainID: &updatedControllerDomainID,
			expectedStatus:             cutil.GetPtr(DomainStatusReady),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := dsd.Update(ctx, tc.tx, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.NotNil(t, got)

				assert.Equal(t, *tc.expectedHostname, got.Hostname)
				assert.Equal(t, *tc.expectedOrg, got.Org)

				assert.Equal(t, tc.expectedControllerDomainID == nil, got.ControllerDomainID == nil)
				if tc.expectedControllerDomainID != nil {
					assert.Equal(t, *tc.expectedControllerDomainID, *got.ControllerDomainID)
				}

				if got.Updated.String() == tc.paramDomain.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}

			}
			if tc.tx != nil {
				assert.Nil(t, tc.tx.Commit())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDomainSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testDomainInitDB(t)
	defer dbSession.Close()
	testDomainSetupSchema(t, dbSession)
	user := testDomainBuildUser(t, dbSession, "testUser")
	controllerDomainID := uuid.New()
	dsd := NewDomainDAO(dbSession)
	domain, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test.com",
		Org:                "testOrg",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	domain2, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test.com",
		Org:                "testOrg",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	tx1, err := db.BeginTx(context.Background(), dbSession, &sql.TxOptions{})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	missingID := uuid.New()

	tests := []struct {
		desc   string
		domain *Domain
		input  DomainClearInput

		expectedUpdate             bool
		expectedError              bool
		expectedControllerDomainID *uuid.UUID
		tx                         *db.Tx
		verifyChildSpanner         bool
	}{
		{
			desc:   "can clear controllerDomainID",
			domain: domain,
			input: DomainClearInput{
				DomainID:           domain.ID,
				ControllerDomainID: true,
			},
			expectedUpdate:             true,
			expectedError:              false,
			expectedControllerDomainID: nil,
			tx:                         tx1,
			verifyChildSpanner:         true,
		},
		{
			desc:   "can clear controllerDomainID when it is already nil",
			domain: domain,
			input: DomainClearInput{
				DomainID:           domain.ID,
				ControllerDomainID: true,
			},
			expectedUpdate:             true,
			expectedError:              false,
			expectedControllerDomainID: nil,
		},
		{
			desc:   "noop when nothing cleared",
			domain: domain2,
			input: DomainClearInput{
				DomainID: domain2.ID,
			},
			expectedUpdate:             false,
			expectedError:              false,
			expectedControllerDomainID: domain2.ControllerDomainID,
		},
		{
			desc:   "error when updating object doesnt exist",
			domain: &Domain{ID: missingID},
			input: DomainClearInput{
				DomainID:           missingID,
				ControllerDomainID: true,
			},
			expectedError:              true,
			expectedControllerDomainID: nil,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := dsd.Clear(ctx, tc.tx, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedControllerDomainID == nil, got.ControllerDomainID == nil)
				if tc.expectedControllerDomainID != nil {
					assert.Equal(t, *tc.expectedControllerDomainID, *got.ControllerDomainID)
				}
				if tc.expectedUpdate {
					assert.True(t, got.Updated.After(tc.domain.Updated))
				}
			}
			if tc.tx != nil {
				assert.Nil(t, tc.tx.Commit())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDomainSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testDomainInitDB(t)
	defer dbSession.Close()
	testDomainSetupSchema(t, dbSession)
	user := testDomainBuildUser(t, dbSession, "testUser")
	controllerDomainID := uuid.New()
	dsd := NewDomainDAO(dbSession)
	domain, err := dsd.Create(ctx, nil, DomainCreateInput{
		Hostname:           "test.com",
		Org:                "testOrg",
		ControllerDomainID: &controllerDomainID,
		Status:             DomainStatusPending,
		CreatedBy:          user.ID,
	})
	assert.Nil(t, err)
	tx1, err := db.BeginTx(context.Background(), dbSession, &sql.TxOptions{})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		expectedError      bool
		tx                 *db.Tx
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			id:                 domain.ID,
			expectedError:      false,
			tx:                 tx1,
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
			err := dsd.Delete(ctx, tc.tx, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := dsd.GetByID(ctx, tc.tx, tc.id, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.tx != nil {
				assert.Nil(t, tc.tx.Commit())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}
