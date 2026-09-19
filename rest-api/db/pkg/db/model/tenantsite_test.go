// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/roles"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestNewTenantSiteDAO(t *testing.T) {
	dbSession := &db.Session{}

	type args struct {
		dbSession *db.Session
	}
	tests := []struct {
		name string
		args args
		want TenantSiteDAO
	}{
		{
			name: "test Tenant Site DAO initialization",
			args: args{
				dbSession: dbSession,
			},
			want: &TenantSiteSQLDAO{
				dbSession:  dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewTenantSiteDAO(tt.args.dbSession)
			assert.Equal(t, got, tt.want)
		})
	}
}

func TestTenantSiteSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{roles.TenantAdminRole}
	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	tnu := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site := TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	ts := TestBuildTenantSite(t, dbSession, tn, site, nil, tnu)

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx              context.Context
		tx               *db.Tx
		id               uuid.UUID
		includeRelations []string
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *TenantSite
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "test get tenant site by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				tx:  nil,
				id:  ts.ID,
			},
			want:               ts,
			verifyChildSpanner: true,
		},
		{
			name: "test get tenant site by ID with relations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				tx:  nil,
				id:  ts.ID,
				includeRelations: []string{
					TenantRelationName,
					SiteRelationName,
				},
			},
			want: ts,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tssd := TenantSiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := tssd.GetByID(tt.args.ctx, tt.args.tx, tt.args.id, tt.args.includeRelations)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			assert.NoError(t, err)

			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.TenantID, got.TenantID)
			assert.Equal(t, tt.want.SiteID, got.SiteID)

			if tt.args.includeRelations != nil {
				assert.NotNil(t, got.Tenant)
				assert.NotNil(t, got.Site)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantSiteSQLDAO_GetByTenantIDAndSiteID(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{roles.TenantAdminRole}
	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	tnu := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site := TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	ts := TestBuildTenantSite(t, dbSession, tn, site, nil, tnu)

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx              context.Context
		tx               *db.Tx
		tenantID         uuid.UUID
		siteID           uuid.UUID
		includeRelations []string
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *TenantSite
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "test get TenantSite by Tenant ID and Site ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      ctx,
				tx:       nil,
				tenantID: tn.ID,
				siteID:   site.ID,
			},
			want:               ts,
			verifyChildSpanner: true,
		},
		{
			name: "test get TenantSite by Tenant ID and Site ID with relations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      ctx,
				tx:       nil,
				tenantID: tn.ID,
				siteID:   site.ID,
				includeRelations: []string{
					TenantRelationName,
					SiteRelationName,
				},
			},
			want: ts,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tssd := TenantSiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := tssd.GetByTenantIDAndSiteID(tt.args.ctx, tt.args.tx, tt.args.tenantID, tt.args.siteID, tt.args.includeRelations)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			assert.NoError(t, err)

			assert.Equal(t, tt.want.TenantID, got.TenantID)
			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, tt.want.ID, got.ID)

			if tt.args.includeRelations != nil {
				assert.NotNil(t, got.Tenant)
				assert.NotNil(t, got.Site)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantSiteSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}
	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnRoles := []string{roles.TenantAdminRole}
	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	tnu1 := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	tn1 := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg1, tnu1)

	tnu2 := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)
	tn2 := TestBuildTenant(t, dbSession, "Test Tenant 2", tnOrg2, tnu2)

	config := &TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)}

	sites := []*Site{}
	siteCount := 30
	for i := range siteCount {
		site := TestBuildSite(t, dbSession, ip, fmt.Sprintf("test-site-%d", i), ipu)
		sites = append(sites, site)
		if i%2 == 0 {
			TestBuildTenantSite(t, dbSession, tn1, site, nil, tnu1)
		} else {
			TestBuildTenantSite(t, dbSession, tn2, site, config, tnu2)
		}
	}

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		tenantIDs        []uuid.UUID
		tenantOrgs       []string
		siteIDs          []uuid.UUID
		configKey        *string
		configVal        *string
		includeRelations []string
		offset           *int
		limit            *int
		orderBy          *paginator.OrderBy
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		verifyChildSpanner bool
	}{
		{
			name: "test get all tenant sites, no filter",
			fields: fields{
				dbSession: dbSession,
			},
			args:               args{},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     siteCount,
			verifyChildSpanner: true,
		},
		{
			name: "test get all tenant sites, filter by tenant ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				tenantIDs: []uuid.UUID{tn1.ID},
			},
			wantCount:      siteCount / 2,
			wantTotalCount: siteCount / 2,
		},
		{
			name: "test get all tenant sites, filter by multiple tenant IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				tenantIDs: []uuid.UUID{tn1.ID, tn2.ID},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: siteCount,
		},
		{
			name: "test get all tenant sites, filter by tenant org",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				tenantOrgs: []string{tnOrg2},
			},
			wantCount:      siteCount / 2,
			wantTotalCount: siteCount / 2,
		},
		{
			name: "test get all tenant sites, filter by multiple tenant org",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				tenantOrgs: []string{tnOrg1, tnOrg2},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: siteCount,
		},
		{
			name: "test get all tenant sites, filter by site ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				siteIDs: []uuid.UUID{sites[0].ID},
			},
			wantCount:      1,
			wantTotalCount: 1,
		},
		{
			name: "test get all tenant sites, filter by multiple site IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				siteIDs: []uuid.UUID{sites[0].ID, sites[1].ID},
			},
			wantCount:      2,
			wantTotalCount: 2,
		},
		{
			name: "test get all tenant sites, filter by config key and value",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				configKey: cutil.GetPtr("targetedInstanceCreation"),
				configVal: cutil.GetPtr("true"),
			},
			wantCount:      siteCount / 2,
			wantTotalCount: siteCount / 2,
		},
		{
			name: "test get all tenant sites, with limit",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				limit: cutil.GetPtr(10),
			},
			wantCount:      10,
			wantTotalCount: siteCount,
		},
		{
			name: "test get all tenant sites, with offset",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				offset: cutil.GetPtr(10),
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: siteCount,
		},
		{
			name: "test get all tenant sites, with order by",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				orderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderDescending,
				},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: siteCount,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tssd := TenantSiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			filter := TenantSiteFilterInput{
				TenantIDs:  tt.args.tenantIDs,
				TenantOrgs: tt.args.tenantOrgs,
				SiteIDs:    tt.args.siteIDs,
				ConfigKey:  tt.args.configKey,
				ConfigVal:  tt.args.configVal,
			}
			page := paginator.PageInput{
				Limit:   tt.args.limit,
				Offset:  tt.args.offset,
				OrderBy: tt.args.orderBy,
			}
			got, count, err := tssd.GetAll(ctx, nil, filter, page, tt.args.includeRelations)

			assert.NoError(t, err)
			assert.Equal(t, tt.wantCount, len(got))
			assert.Equal(t, tt.wantTotalCount, count)

			if tt.args.orderBy != nil {
				assert.Equal(t, sites[siteCount-1].ID, got[0].SiteID)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantSiteSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{roles.TenantAdminRole}
	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	tnu := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site := TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		tenantID  uuid.UUID
		tenantOrg string
		siteID    uuid.UUID
		config    *TenantSiteConfig
		createdBy uuid.UUID
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *TenantSite
		verifyChildSpanner bool
	}{
		{
			name: "test create tenant site",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				tenantID:  tn.ID,
				tenantOrg: tnOrg,
				siteID:    site.ID,
				config:    &TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
				createdBy: tnu.ID,
			},
			want: &TenantSite{
				TenantID:  tn.ID,
				TenantOrg: tnOrg,
				SiteID:    site.ID,
				Config:    TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
				CreatedBy: tnu.ID,
			},
			verifyChildSpanner: true,
		},
		{
			name: "test create tenant site - nil config",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				tenantID:  tn.ID,
				tenantOrg: tnOrg,
				siteID:    site.ID,
				config:    nil,
				createdBy: tnu.ID,
			},
			want: &TenantSite{
				TenantID:  tn.ID,
				TenantOrg: tnOrg,
				SiteID:    site.ID,
				Config:    TenantSiteConfig{},
				CreatedBy: tnu.ID,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tssd := TenantSiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			input := TenantSiteCreateInput{
				TenantID:  tt.args.tenantID,
				TenantOrg: tt.args.tenantOrg,
				SiteID:    tt.args.siteID,
				Config:    tt.args.config,
				CreatedBy: tt.args.createdBy,
			}
			got, err := tssd.Create(ctx, nil, input)
			assert.NoError(t, err)

			assert.NotNil(t, got.ID)
			assert.Equal(t, tt.want.TenantID, got.TenantID)
			assert.Equal(t, tt.want.TenantOrg, got.TenantOrg)
			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, tt.want.Config, got.Config)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantSiteSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{roles.TenantAdminRole}
	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	tnu := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site := TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	ts := TestBuildTenantSite(t, dbSession, tn, site, nil, tnu)

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		id                  uuid.UUID
		enableSerialConsole *bool
		config              *TenantSiteConfig
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *TenantSite
		verifyChildSpanner bool
	}{
		{
			name: "test update tenant site, update enable serial console",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				id:                  ts.ID,
				enableSerialConsole: cutil.GetPtr(true),
			},
			want: &TenantSite{
				ID:                  ts.ID,
				EnableSerialConsole: true,
			},
			verifyChildSpanner: true,
		},
		{
			name: "test update tenant site, update config",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				id:     ts.ID,
				config: &TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
			},
			want: &TenantSite{
				ID:     ts.ID,
				Config: TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tssd := TenantSiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			input := TenantSiteUpdateInput{
				TenantSiteID:        tt.args.id,
				EnableSerialConsole: tt.args.enableSerialConsole,
				Config:              tt.args.config,
			}
			got, err := tssd.Update(ctx, nil, input)
			assert.NoError(t, err)

			if tt.args.enableSerialConsole != nil {
				assert.Equal(t, tt.want.EnableSerialConsole, got.EnableSerialConsole)
			}

			if tt.args.config != nil {
				assert.Equal(t, tt.want.Config, got.Config)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

	t.Run("remove targetedInstanceCreation", func(t *testing.T) {
		tsWithOverride := TestBuildTenantSite(t, dbSession, tn, TestBuildSite(t, dbSession, ip, "Test Site 2", ipu),
			&TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)}, tnu)

		tssd := TenantSiteSQLDAO{dbSession: dbSession}
		got, err := tssd.Update(ctx, nil, TenantSiteUpdateInput{
			TenantSiteID: tsWithOverride.ID,
			Config:       &TenantSiteConfig{},
		})
		assert.NoError(t, err)
		assert.Nil(t, got.Config.TargetedInstanceCreation)
	})

}

func TestTenantSiteSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{roles.TenantAdminRole}
	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	tnu := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site := TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	ts := TestBuildTenantSite(t, dbSession, tn, site, nil, tnu)

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		id uuid.UUID
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		verifyChildSpanner bool
	}{
		{
			name: "test delete tenant site",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				id: ts.ID,
			},
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tssd := TenantSiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			err := tssd.Delete(ctx, nil, tt.args.id)
			assert.NoError(t, err)

			// Check if the tenant site is deleted
			_, err = tssd.GetByID(ctx, nil, tt.args.id, nil)
			assert.ErrorIs(t, err, db.ErrDoesNotExist)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
