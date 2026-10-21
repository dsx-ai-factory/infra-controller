// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
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

func testTenantAccountInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for TenantAccount tests
func testTenantAccountSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create TenantAccount table
	err = dbSession.DB.ResetModel(context.Background(), (*TenantAccount)(nil))
	assert.Nil(t, err)
}

func testTenantAccountBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
	ip := &InfrastructureProvider{
		ID:             uuid.New(),
		Name:           name,
		DisplayName:    cutil.GetPtr("Test Provider"),
		Org:            "test",
		OrgDisplayName: cutil.GetPtr("Test Org"),
		CreatedBy:      uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testTenantAccountBuildTenant(t *testing.T, dbSession *db.Session, name string, org string) *Tenant {
	tenant := &Tenant{
		ID:             uuid.New(),
		Name:           name,
		DisplayName:    cutil.GetPtr("Test Tenant"),
		Org:            org,
		OrgDisplayName: cutil.GetPtr(name + "-display"),
		CreatedBy:      uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testTenantAccountBuildUser(t *testing.T, dbSession *db.Session, starfleetID string, firstName string, lastName string) *User {
	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(starfleetID),
		Email:       cutil.GetPtr(fmt.Sprintf("%s.%s@test.com", firstName, lastName)),
		FirstName:   cutil.GetPtr(firstName),
		LastName:    cutil.GetPtr(lastName),
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	assert.Nil(t, err)
	return user
}

func TestTenantAccountSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)
	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant := testTenantAccountBuildTenant(t, dbSession, "testTenant", "test-tenant-org")
	user := testTenantAccountBuildUser(t, dbSession, "testUser", "John", "Doe")
	tasd := NewTenantAccountDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		tas                []TenantAccount
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one with Tenant ID",
			tas: []TenantAccount{
				{
					AccountNumber:             "123",
					InfrastructureProviderID:  ip.ID,
					InfrastructureProviderOrg: ip.Org,
					TenantID:                  &tenant.ID,
					TenantOrg:                 tenant.Org,
					Status:                    TenantAccountStatusPending,
					CreatedBy:                 user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create one with Tenant Org",
			tas: []TenantAccount{
				{
					AccountNumber:             "321",
					InfrastructureProviderID:  ip.ID,
					InfrastructureProviderOrg: ip.Org,
					TenantOrg:                 tenant.Org,
					Status:                    TenantAccountStatusPending,
					CreatedBy:                 user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "error when duplicate account number",
			tas: []TenantAccount{
				{
					AccountNumber:            "123",
					InfrastructureProviderID: ip.ID,
					TenantID:                 &tenant.ID,
					Status:                   TenantAccountStatusPending,
					CreatedBy:                user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "create multiple",
			tas: []TenantAccount{
				{
					AccountNumber:            "456",
					InfrastructureProviderID: ip.ID,
					TenantID:                 &tenant.ID,
					Status:                   TenantAccountStatusPending,
					CreatedBy:                user.ID,
				},
				{
					AccountNumber:            "789",
					InfrastructureProviderID: ip.ID,
					TenantID:                 &tenant.ID,
					Status:                   TenantAccountStatusPending,
					CreatedBy:                user.ID,
				},
				{
					AccountNumber:            "1011",
					InfrastructureProviderID: ip.ID,
					TenantID:                 &tenant.ID,
					Status:                   TenantAccountStatusPending,
					CreatedBy:                user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "error - foreign key violation on tenant_id",
			tas: []TenantAccount{
				{
					AccountNumber:            "1213",
					InfrastructureProviderID: ip.ID,
					TenantID:                 cutil.GetPtr(uuid.New()),
					Status:                   TenantAccountStatusPending,
					CreatedBy:                user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.tas {
				ta, err := tasd.Create(
					ctx, nil, TenantAccountCreateInput{
						AccountNumber:             i.AccountNumber,
						TenantID:                  i.TenantID,
						TenantOrg:                 i.TenantOrg,
						InfrastructureProviderID:  i.InfrastructureProviderID,
						InfrastructureProviderOrg: i.InfrastructureProviderOrg,
						SubscriptionID:            i.SubscriptionID,
						SubscriptionTier:          i.SubscriptionTier,
						Status:                    i.Status,
						CreatedBy:                 i.CreatedBy,
					})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.Nil(t, err)
					assert.NotNil(t, ta)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestTenantAccountSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)
	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "testIP")
	tn := testTenantAccountBuildTenant(t, dbSession, "testTenant", "test-tenant-org")
	user := testTenantAccountBuildUser(t, dbSession, "testUser", "John", "Doe")
	tasd := NewTenantAccountDAO(dbSession)
	ta, err := tasd.Create(
		ctx, nil, TenantAccountCreateInput{
			AccountNumber:             "123",
			TenantID:                  &tn.ID,
			TenantOrg:                 tn.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("subsid"),
			SubscriptionTier:          cutil.GetPtr("subtier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta)
	ta2, err := tasd.Create(
		ctx, nil, TenantAccountCreateInput{
			AccountNumber:             "456",
			TenantID:                  &tn.ID,
			TenantOrg:                 tn.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("subsid"),
			SubscriptionTier:          cutil.GetPtr("subtier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta2)
	ta2, err = tasd.Update(ctx, nil, TenantAccountUpdateInput{
		TenantAccountID:  ta2.ID,
		SubscriptionID:   nil,
		SubscriptionTier: nil,
		TenantID:         nil,
		TenantContactID:  &user.ID,
		Status:           cutil.GetPtr(TenantAccountStatusReady),
	})
	assert.Nil(t, err)
	assert.NotNil(t, ta)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                    string
		id                      uuid.UUID
		expectedError           bool
		expectedErrVal          error
		includedRelations       []string
		expectedTenantContactID *uuid.UUID
		verifyChildSpanner      bool
	}{
		{
			desc:                    "GetById success when TenantAccount exists and tenantcontact is nil",
			id:                      ta.ID,
			expectedError:           false,
			includedRelations:       []string{},
			expectedTenantContactID: nil,
			verifyChildSpanner:      true,
		},
		{
			desc:                    "ok when tenantcontact is not nil and GetByID expands it",
			id:                      ta2.ID,
			expectedError:           false,
			includedRelations:       []string{"TenantContact"},
			expectedTenantContactID: &user.ID,
		},
		{
			desc:           "GetById error when not found",
			id:             uuid.New(),
			expectedError:  true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := tasd.GetByID(ctx, nil, tc.id, tc.includedRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.id, got.ID)
				assert.Equal(t, tc.expectedTenantContactID, got.TenantContactID)
			}
			if tc.expectedTenantContactID != nil {
				assert.Equal(t, *tc.expectedTenantContactID, got.TenantContact.ID)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountSQLDAO_GetCountByStatus(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)
	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "testIP")
	tn := testTenantAccountBuildTenant(t, dbSession, "testTenant", "test-tenant-org")
	user := testTenantAccountBuildUser(t, dbSession, "testUser", "John", "Doe")
	tasd := NewTenantAccountDAO(dbSession)
	ta, err := tasd.Create(
		ctx, nil, TenantAccountCreateInput{
			AccountNumber:             "123",
			TenantID:                  &tn.ID,
			TenantOrg:                 tn.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("subsid"),
			SubscriptionTier:          cutil.GetPtr("subtier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta)
	ta2, err := tasd.Create(
		ctx, nil, TenantAccountCreateInput{
			AccountNumber:             "456",
			TenantID:                  &tn.ID,
			TenantOrg:                 tn.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("subsid"),
			SubscriptionTier:          cutil.GetPtr("subtier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta2)
	ta2, err = tasd.Update(ctx, nil, TenantAccountUpdateInput{
		TenantAccountID:  ta2.ID,
		SubscriptionID:   nil,
		SubscriptionTier: nil,
		TenantID:         nil,
		TenantContactID:  &user.ID,
		Status:           cutil.GetPtr(TenantAccountStatusReady),
	})
	assert.Nil(t, err)
	assert.NotNil(t, ta2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		id                 uuid.UUID
		fields             fields
		args               args
		wantErr            error
		wantEmpty          bool
		wantCount          int
		wantStatusMap      map[string]int
		reqIP              *uuid.UUID
		reqTenant          *uuid.UUID
		verifyChildSpanner bool
	}{
		{
			name: "get tenantaccount status count by infrastructure provider with tenantaccount returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 2,
			wantStatusMap: map[string]int{
				TenantAccountStatusInvited: 0,
				TenantAccountStatusError:   0,
				TenantAccountStatusPending: 1,
				TenantAccountStatusReady:   1,
				"total":                    2,
			},
			reqIP:              cutil.GetPtr(ip.ID),
			verifyChildSpanner: true,
		},
		{
			name: "get tenantaccount status count by tenant with tenantaccount returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 2,
			wantStatusMap: map[string]int{
				TenantAccountStatusInvited: 0,
				TenantAccountStatusError:   0,
				TenantAccountStatusPending: 1,
				TenantAccountStatusReady:   1,
				"total":                    2,
			},
			reqTenant: cutil.GetPtr(tn.ID),
		},
		{
			name: "get tenantaccount status count by unexisted infrastructure provider with no tenantaccount returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: true,
			wantCount: 0,
			reqIP:     cutil.GetPtr(uuid.New()),
		},
		{
			name: "get tenantaccount status count with no filter tenantaccount returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantCount: 2,
			wantStatusMap: map[string]int{
				TenantAccountStatusInvited: 0,
				TenantAccountStatusError:   0,
				TenantAccountStatusPending: 1,
				TenantAccountStatusReady:   1,
				"total":                    2,
			},
			wantEmpty: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tasd := TenantAccountSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := tasd.GetCountByStatus(tt.args.ctx, nil, tt.reqIP, tt.reqTenant)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if tt.wantEmpty {
				assert.EqualValues(t, got["total"], 0)
			}
			if err == nil && !tt.wantEmpty {
				assert.EqualValues(t, tt.wantStatusMap, got)
				if len(got) > 0 {
					assert.EqualValues(t, got[TenantAccountStatusPending], 1)
					assert.EqualValues(t, got["total"], tt.wantCount)
				}
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)

	ip1 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "test-provider-1")
	ip2 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "test-provider-2")

	createUser := testTenantAccountBuildUser(t, dbSession, "testUser1", "John", "Doe")
	contactUser1 := testTenantAccountBuildUser(t, dbSession, "testUser2", "John", "Smith")
	contactUser2 := testTenantAccountBuildUser(t, dbSession, "testUser3", "John", "Brewer")
	tasd := NewTenantAccountDAO(dbSession)

	totalCount := 30

	tas := []TenantAccount{}

	for i := 0; i < totalCount; i++ {
		tenant := testTenantAccountBuildTenant(t, dbSession, fmt.Sprintf("test-tenant-%v", i), fmt.Sprintf("test-tenant-org-%v", i))

		var ta *TenantAccount
		var err error

		if i%2 == 0 {
			ta, err = tasd.Create(
				ctx, nil, TenantAccountCreateInput{
					AccountNumber:             fmt.Sprintf("account-number-%v", i),
					TenantID:                  &tenant.ID,
					TenantOrg:                 tenant.Org,
					InfrastructureProviderID:  ip1.ID,
					InfrastructureProviderOrg: ip1.Org,
					SubscriptionID:            cutil.GetPtr("subsid"),
					SubscriptionTier:          cutil.GetPtr("subtier"),
					Status:                    TenantAccountStatusPending,
					CreatedBy:                 createUser.ID,
				},
			)
			assert.NoError(t, err)
			ta, err = tasd.Update(ctx, nil, TenantAccountUpdateInput{TenantAccountID: ta.ID, TenantContactID: &contactUser1.ID})
			assert.NoError(t, err)
		} else {
			ta, err = tasd.Create(
				ctx, nil, TenantAccountCreateInput{
					AccountNumber:             fmt.Sprintf("account-number-%v", i),
					TenantID:                  &tenant.ID,
					TenantOrg:                 tenant.Org,
					InfrastructureProviderID:  ip2.ID,
					InfrastructureProviderOrg: ip2.Org,
					SubscriptionID:            cutil.GetPtr("subsid"),
					SubscriptionTier:          cutil.GetPtr("subtier"),
					Status:                    TenantAccountStatusPending,
					CreatedBy:                 createUser.ID,
				},
			)
			assert.NoError(t, err)
			ta, err = tasd.Update(ctx, nil, TenantAccountUpdateInput{TenantAccountID: ta.ID, TenantContactID: &contactUser2.ID})
			assert.NoError(t, err)
		}

		ta.Tenant = tenant
		tas = append(tas, *ta)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             TenantAccountFilterInput
		offset             *int
		limit              *int
		orderBy            *paginator.OrderBy
		firstEntry         *TenantAccount
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		includeRelations   []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			filter:             TenantAccountFilterInput{},
			includeRelations:   []string{"TenantContact"},
			expectedCount:      paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetAll with whitespace-only search query returns objects",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("   "),
			},
			includeRelations: []string{"TenantContact"},
			expectedCount:    paginator.DefaultLimit,
			expectedTotal:    &totalCount,
			expectedError:    false,
		},
		{
			desc: "GetAll with Tenant ID filter returns objects",
			filter: TenantAccountFilterInput{
				TenantIDs: []uuid.UUID{*tas[0].TenantID},
			},
			includeRelations: []string{"TenantContact"},
			expectedCount:    1,
			expectedError:    false,
		},
		{
			desc: "GetAll with Tenant org filter returns objects",
			filter: TenantAccountFilterInput{
				TenantOrgs: []string{tas[0].Tenant.Org},
			},
			includeRelations: []string{"TenantContact"},
			expectedCount:    1,
			expectedError:    false,
		},
		{
			desc: "GetAll with non-existent Infrastructure Provider ID filter returns no objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: cutil.GetPtr(uuid.New()),
			},
			includeRelations: []string{"TenantContact"},
			expectedCount:    0,
			expectedError:    false,
		},
		{
			desc: "GetAll with Infrastructure Provider ID filter returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip1.ID,
			},
			includeRelations: []string{"TenantContact"},
			expectedCount:    totalCount / 2,
			expectedError:    false,
		},
		{
			desc: "GetAll with both Infrastructure Provider ID and Tenant ID filter returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip2.ID,
				TenantIDs:                []uuid.UUID{*tas[1].TenantID},
			},
			includeRelations: []string{"TenantContact"},
			expectedCount:    1,
			expectedError:    false,
		},
		{
			desc: "GetAll with limit returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip1.ID,
			},
			offset:        cutil.GetPtr(0),
			limit:         cutil.GetPtr(5),
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip1.ID,
			},
			offset:        cutil.GetPtr(5),
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip1.ID,
			},
			orderBy: &paginator.OrderBy{
				Field: "account_number",
				Order: paginator.OrderDescending,
			},
			firstEntry:    &tas[8], // 5th entry is "subnet-8" and would appear first on descending order
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with status returns objects",
			filter: TenantAccountFilterInput{
				Statuses: []string{*cutil.GetPtr(TenantAccountStatusPending)},
			},
			offset:        cutil.GetPtr(5),
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc: "GetAll with order by tenant name no tenant relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantOrgNameExt,
				Order: paginator.OrderDescending,
			},
			firstEntry:    &tas[9],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
		},
		{
			desc: "GetAll with order by tenant name and tenant relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantOrgNameExt,
				Order: paginator.OrderDescending,
			},
			firstEntry:       &tas[9],
			expectedCount:    20,
			expectedTotal:    cutil.GetPtr(totalCount),
			includeRelations: []string{TenantRelationName},
		},
		{
			desc: "GetAll with order by tenant display name no tenant relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantOrgDisplayNameExt,
				Order: paginator.OrderDescending,
			},
			firstEntry:    &tas[9],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
		},
		{
			desc: "GetAll with order by tenant display name and tenant relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantOrgDisplayNameExt,
				Order: paginator.OrderDescending,
			},
			firstEntry:       &tas[9],
			expectedCount:    20,
			expectedTotal:    cutil.GetPtr(totalCount),
			includeRelations: []string{TenantRelationName},
		},
		{
			desc: "GetAll with order by tenant contact email no tenant contact relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantContactEmailExt,
				Order: paginator.OrderAscending,
			},
			firstEntry:    &tas[29],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
		},
		{
			desc: "GetAll with order by tenant contact email and tenant contact relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantContactEmailExt,
				Order: paginator.OrderAscending,
			},
			firstEntry:       &tas[29],
			expectedCount:    20,
			expectedTotal:    cutil.GetPtr(totalCount),
			includeRelations: []string{TenantContactRelationName},
		},
		{
			desc: "GetAll with order by tenant contact full name no tenant contact relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantContactFullNameExt,
				Order: paginator.OrderAscending,
			},
			firstEntry:    &tas[29],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
		},
		{
			desc: "GetAll with order by tenant contact full name and tenant contact relation",
			orderBy: &paginator.OrderBy{
				Field: tenantAccountOrderByTenantContactFullNameExt,
				Order: paginator.OrderAscending,
			},
			firstEntry:       &tas[29],
			expectedCount:    20,
			expectedTotal:    cutil.GetPtr(totalCount),
			includeRelations: []string{TenantContactRelationName},
		},
		{
			desc: "GetAll with search query matching account_number",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("account-number-0"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query matching tenant_org",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("test-tenant-org-29"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query matching tenant org_display_name",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("test-tenant-5-display"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query no matches",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("nonexistent-query-xyz"),
			},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc: "GetAll with search query case insensitive",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("ACCOUNT-NUMBER-0"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query combined with status filter",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("account-number"),
				Statuses:    []string{TenantAccountStatusPending},
			},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := tasd.GetAll(ctx, nil, tc.filter, paginator.PageInput{Offset: tc.offset, Limit: tc.limit, OrderBy: tc.orderBy}, tc.includeRelations)
			if err != nil {
				fmt.Println(err.Error())
			}

			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.AccountNumber, got[0].AccountNumber)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountSQLDAO_GetCount(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)

	ip1 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "test-provider-1")
	ip2 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "test-provider-2")

	user := testTenantAccountBuildUser(t, dbSession, "testUser", "John", "Doe")
	tasd := NewTenantAccountDAO(dbSession)

	totalCount := 30

	tas := []TenantAccount{}

	for i := 0; i < totalCount; i++ {
		tenant := testTenantAccountBuildTenant(t, dbSession, fmt.Sprintf("test-tenant-%v", i), fmt.Sprintf("test-tenant-org-%v", i))

		var ta *TenantAccount
		var err error

		if i%2 == 0 {
			ta, err = tasd.Create(
				ctx, nil, TenantAccountCreateInput{
					AccountNumber:             fmt.Sprintf("account-number-%v", i),
					TenantID:                  &tenant.ID,
					TenantOrg:                 tenant.Org,
					InfrastructureProviderID:  ip1.ID,
					InfrastructureProviderOrg: ip1.Org,
					SubscriptionID:            cutil.GetPtr("subsid"),
					SubscriptionTier:          cutil.GetPtr("subtier"),
					Status:                    TenantAccountStatusPending,
					CreatedBy:                 user.ID,
				},
			)
			assert.NoError(t, err)
		} else {
			ta, err = tasd.Create(
				ctx, nil, TenantAccountCreateInput{
					AccountNumber:             fmt.Sprintf("account-number-%v", i),
					TenantID:                  &tenant.ID,
					TenantOrg:                 tenant.Org,
					InfrastructureProviderID:  ip2.ID,
					InfrastructureProviderOrg: ip2.Org,
					SubscriptionID:            cutil.GetPtr("subsid"),
					SubscriptionTier:          cutil.GetPtr("subtier"),
					Status:                    TenantAccountStatusPending,
					CreatedBy:                 user.ID,
				},
			)
			assert.NoError(t, err)
		}

		ta.Tenant = tenant
		tas = append(tas, *ta)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             TenantAccountFilterInput
		expectedCount      int
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetCount with no filters returns objects",
			filter:             TenantAccountFilterInput{},
			verifyChildSpanner: true,
			expectedCount:      totalCount,
			expectedError:      false,
		},
		{
			desc: "GetCount with Tenant ID filter returns objects",
			filter: TenantAccountFilterInput{
				TenantIDs: []uuid.UUID{*tas[0].TenantID},
			},
			expectedCount: 1,
		},
		{
			desc: "GetCount with Tenant org filter returns objects",
			filter: TenantAccountFilterInput{
				TenantOrgs: []string{tas[0].Tenant.Org},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetCount with non-existent Infrastructure Provider ID filter returns no objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: cutil.GetPtr(uuid.New()),
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetCount with Infrastructure Provider ID filter returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip1.ID,
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetCount with both Infrastructure Provider ID and Tenant ID filter returns objects",
			filter: TenantAccountFilterInput{
				InfrastructureProviderID: &ip2.ID,
				TenantIDs:                []uuid.UUID{*tas[1].TenantID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetCount with status returns objects",
			filter: TenantAccountFilterInput{
				Statuses: []string{*cutil.GetPtr(TenantAccountStatusPending)},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with search query and status filter combined",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("account-number-0"),
				Statuses:    []string{TenantAccountStatusPending},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetCount with search query no matches",
			filter: TenantAccountFilterInput{
				SearchQuery: cutil.GetPtr("nonexistent-query-xyz"),
			},
			expectedCount: 0,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			count, err := tasd.GetCount(ctx, nil, tc.filter)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, count)
			} else {
				assert.Equal(t, tc.expectedCount, count)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)
	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "testIP")
	tn := testTenantAccountBuildTenant(t, dbSession, "testTenant", "test-tenant-org")
	tn2 := testTenantAccountBuildTenant(t, dbSession, "testTenant2", "test-tenant-org-2")

	user := testTenantAccountBuildUser(t, dbSession, "testUser", "John", "Does")
	user2 := testTenantAccountBuildUser(t, dbSession, "testUser2", "Mark", "Smith")

	tasd := NewTenantAccountDAO(dbSession)
	ta, err := tasd.Create(
		ctx, nil,
		TenantAccountCreateInput{
			AccountNumber:             uuid.NewString(),
			TenantID:                  &tn.ID,
			TenantOrg:                 tn.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("SubscriptionID"),
			SubscriptionTier:          cutil.GetPtr("SubscriptionTier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta)

	ta2, err := tasd.Create(
		ctx, nil, TenantAccountCreateInput{
			AccountNumber:             uuid.NewString(),
			TenantID:                  nil,
			TenantOrg:                 tn2.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("SubscriptionID"),
			SubscriptionTier:          cutil.GetPtr("SubscriptionTier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		ta   *TenantAccount

		paramTenantID         *uuid.UUID
		paramSubscriptionID   *string
		paramSubscriptionTier *string
		paramTenantContactID  *uuid.UUID
		paramStatus           *string

		expectedError            bool
		expectedTenantID         *uuid.UUID
		expectedSubscriptionID   *string
		expectedSubscriptionTier *string
		expectedTenantContactID  *uuid.UUID
		expectedStatus           *string
		verifyChildSpanner       bool
	}{
		{
			desc: "can update subscript ID, tier and status",
			ta:   ta,

			paramTenantID:         nil,
			paramSubscriptionID:   cutil.GetPtr("updatedSubscriptionID"),
			paramSubscriptionTier: cutil.GetPtr("updatedSubscriptionTier"),
			paramTenantContactID:  nil,
			paramStatus:           cutil.GetPtr(TenantAccountStatusReady),

			expectedTenantID:         ta.TenantID,
			expectedSubscriptionID:   cutil.GetPtr("updatedSubscriptionID"),
			expectedSubscriptionTier: cutil.GetPtr("updatedSubscriptionTier"),
			expectedTenantContactID:  ta.TenantContactID,
			expectedStatus:           cutil.GetPtr(TenantAccountStatusReady),
			verifyChildSpanner:       true,
		},
		{
			desc: "can update Tenant Contact ID",
			ta:   ta,

			paramTenantID:         nil,
			paramSubscriptionID:   nil,
			paramSubscriptionTier: nil,
			paramTenantContactID:  cutil.GetPtr(user2.ID),
			paramStatus:           nil,

			expectedTenantID:         ta.TenantID,
			expectedSubscriptionID:   cutil.GetPtr("updatedSubscriptionID"),
			expectedSubscriptionTier: cutil.GetPtr("updatedSubscriptionTier"),
			expectedTenantContactID:  cutil.GetPtr(user2.ID),
			expectedStatus:           cutil.GetPtr(TenantAccountStatusReady),
		},
		{
			desc: "can update Tenant ID",
			ta:   ta2,

			paramTenantID:         &tn2.ID,
			paramSubscriptionID:   nil,
			paramSubscriptionTier: nil,
			paramTenantContactID:  ta2.TenantContactID,
			paramStatus:           nil,

			expectedTenantID:         &tn2.ID,
			expectedSubscriptionID:   cutil.GetPtr("SubscriptionID"),
			expectedSubscriptionTier: cutil.GetPtr("SubscriptionTier"),
			expectedTenantContactID:  ta2.TenantContactID,
			expectedStatus:           cutil.GetPtr(TenantAccountStatusPending),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := tasd.Update(ctx, nil, TenantAccountUpdateInput{
				TenantAccountID:  tc.ta.ID,
				TenantID:         tc.paramTenantID,
				SubscriptionID:   tc.paramSubscriptionID,
				SubscriptionTier: tc.paramSubscriptionTier,
				TenantContactID:  tc.paramTenantContactID,
				Status:           tc.paramStatus,
			})
			assert.Nil(t, err)
			assert.NotNil(t, got)

			if tc.expectedTenantID != nil {
				assert.Equal(t, *tc.expectedTenantID, *got.TenantID)
			}

			if tc.expectedSubscriptionID != nil {
				assert.Equal(t, *tc.expectedSubscriptionID, *got.SubscriptionID)
			}

			if tc.expectedSubscriptionTier != nil {
				assert.Equal(t, *tc.expectedSubscriptionTier, *got.SubscriptionTier)
			}

			if tc.expectedTenantContactID != nil {
				assert.Equal(t, *tc.expectedTenantContactID, *got.TenantContactID)
			}
			assert.Equal(t, *tc.expectedStatus, got.Status)

			if got.Updated.String() == tc.ta.Updated.String() {
				t.Errorf("got.Updated = %v, want different value", got.Updated)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()
	testTenantAccountSetupSchema(t, dbSession)
	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "testIP")
	tn := testTenantAccountBuildTenant(t, dbSession, "testTenant", "test-tenant-org")
	user := testTenantAccountBuildUser(t, dbSession, "testUser", "John", "Doe")
	tasd := NewTenantAccountDAO(dbSession)
	ta, err := tasd.Create(
		ctx, nil, TenantAccountCreateInput{
			AccountNumber:             "123",
			TenantID:                  &tn.ID,
			TenantOrg:                 tn.Org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			SubscriptionID:            cutil.GetPtr("subsid"),
			SubscriptionTier:          cutil.GetPtr("subtier"),
			Status:                    TenantAccountStatusPending,
			CreatedBy:                 user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ta)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		ipbID              uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			ipbID:              ta.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			ipbID:         uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tasd.Delete(ctx, nil, tc.ipbID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := tasd.GetByID(ctx, nil, tc.ipbID, nil)
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
