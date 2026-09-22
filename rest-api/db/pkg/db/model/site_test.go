// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/uptrace/bun/extra/bundebug"
	otrace "go.opentelemetry.io/otel/trace"
)

func setupSchema(t *testing.T, dbSession *db.Session) {
	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	// Create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	if err != nil {
		t.Fatal(err)
	}

	// Add tsv index with new columns
	_, err = dbSession.DB.Exec("CREATE INDEX site_tsv_idx ON site USING gin(to_tsvector('english', name || ' ' || description || ' ' || status || ' ' || location::text || ' ' || contact::text))")
	if err != nil {
		t.Fatal(err)
	}
}

func TestSiteSQLDAO_GetByID(t *testing.T) {
	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// time.Now sets a timezone, but reading back from postgres doesn't load a TZ for zero offset
	expirationTime := db.GetCurTime()

	// Create site
	st1 := &Site{
		ID:                          uuid.New(),
		Name:                        "test-site-1",
		DisplayName:                 cutil.GetPtr("Test Site 1"),
		Org:                         "test-org",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(expirationTime),
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}

	_, err = dbSession.DB.NewInsert().Model(st1).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// Create deleted site
	st2 := &Site{
		ID:                       uuid.New(),
		Name:                     "test-site-2",
		DisplayName:              cutil.GetPtr("Test"),
		Org:                      "test-org",
		InfrastructureProviderID: ip.ID,
		Status:                   SiteStatusRegistered,
		CreatedBy:                uuid.New(),
		Deleted:                  cutil.GetPtr(time.Now().Add(time.Hour * 24)),
	}

	_, err = dbSession.DB.NewInsert().Model(st2).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx              context.Context
		id               uuid.UUID
		includeRelations bool
		includeDeleted   bool
	}

	// Define tests
	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *Site
		wantErr            bool
		wantErrVal         error
		verifyChildSpanner bool
	}{
		{
			name: "get a Site by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:              ctx,
				id:               st1.ID,
				includeRelations: false,
			},
			want:               st1,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "error getting a Site by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:              context.Background(),
				id:               uuid.New(),
				includeRelations: false,
			},
			want:       st1,
			wantErr:    true,
			wantErrVal: db.ErrDoesNotExist,
		},
		{
			name: "get a Site by ID and include relations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:              context.Background(),
				id:               st1.ID,
				includeRelations: true,
			},
			want:    st1,
			wantErr: false,
		},
		{
			name: "get a deleted Site by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            context.Background(),
				id:             st2.ID,
				includeDeleted: true,
			},
			want: st2,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			relations := []string{}
			if tt.args.includeRelations {
				relations = append(relations, InfrastructureProviderRelationName)
				tt.want.InfrastructureProvider = ip
			} else {
				tt.want.InfrastructureProvider = nil
			}

			got, err := ssd.GetByID(tt.args.ctx, nil, tt.args.id, relations, tt.args.includeDeleted)
			if (err != nil) != tt.wantErr {
				t.Errorf("SiteSQLDAO.GetByID()\nerror = %v,\nwantErr = %v", err, tt.wantErr)
				return
			}
			if tt.wantErr {
				assert.Equal(t, tt.wantErrVal, err)
				return
			}
			if got.ID != tt.want.ID {
				t.Errorf("SiteSQLDAO.GetByID()\ngotID = %v,\nwantID = %v", got.ID, tt.want.ID)
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSiteSQLDAO_GetAll(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx                    context.Context
		filter                 SiteFilterInput
		page                   paginator.PageInput
		infrastructureProvider *InfrastructureProvider
		firstEntry             *Site
		includeRelations       bool
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test-org",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// time.Now sets a timezone, but reading back from postgres doesn't load a TZ for zero offset
	expirationTime := db.GetCurTime()

	// Create sites
	sites := []Site{}
	for i := 25; i > 0; i-- {
		site := &Site{
			ID:                          uuid.New(),
			Name:                        fmt.Sprintf("test%v", i),
			Description:                 cutil.GetPtr(fmt.Sprintf("test-description-%v", i)),
			DisplayName:                 cutil.GetPtr(fmt.Sprintf("Test %v", i)),
			Org:                         ip.Org,
			InfrastructureProviderID:    ip.ID,
			SiteControllerVersion:       cutil.GetPtr("1.0.0"),
			SiteAgentVersion:            cutil.GetPtr("1.0.0"),
			RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
			RegistrationTokenExpiration: cutil.GetPtr(expirationTime),
			SerialConsoleHostname:       cutil.GetPtr(fmt.Sprintf("test-serial-console-hostname%v", i)),
			Status:                      SiteStatusPending,
			CreatedBy:                   uuid.New(),
			Config:                      &SiteConfig{},
		}

		if i == 25 || i == 24 {
			site.Config.NativeNetworking = true
			site.Config.VpcSlaac = true
			site.Config.NVLinkPartition = true
			site.Config.Flow = true
		}

		if i == 23 {
			site.Config.NetworkSecurityGroup = true
		}

		if i == 22 {
			site.Config.NetworkSecurityGroup = true
			site.Config.NativeNetworking = true
			site.Config.NVLinkPartition = true
			site.Config.MaxNetworkSecurityGroupRuleCount = cutil.GetPtr(100)
		}

		_, err = dbSession.DB.NewInsert().Model(site).Exec(context.Background())
		if err != nil {
			t.Fatal(err)
		}

		sites = append(sites, *site)
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get all Sites by Infrastructure Provider ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: SiteFilterInput{
					Name:                      nil,
					Org:                       nil,
					InfrastructureProviderIDs: []uuid.UUID{ip.ID},
					SiteIDs:                   nil,
				},
				infrastructureProvider: ip,
				includeRelations:       true,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     len(sites),
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by name and include relations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Name:                      cutil.GetPtr("test1"),
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
				},
				infrastructureProvider: ip,
				includeRelations:       true,
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantErr:        false,
		},
		{
			name: "get all Sites by Org",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       &ip.Org,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites by set of IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   []uuid.UUID{sites[0].ID, sites[1].ID},
				},
				includeRelations: false,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Sites by native networking flag",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config:                    &SiteConfigFilterInput{NativeNetworking: cutil.GetPtr(true)},
				},
				includeRelations: false,
			},
			wantCount:      3,
			wantTotalCount: 3,
			wantErr:        false,
		},
		{
			name: "get all Sites by VPC SLAAC flag",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config:                    &SiteConfigFilterInput{VpcSlaac: cutil.GetPtr(true)},
				},
				includeRelations: false,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Sites by NVLink partitioning flag",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config:                    &SiteConfigFilterInput{NVLinkPartition: cutil.GetPtr(true)},
				},
				includeRelations: false,
			},
			wantCount:      3,
			wantTotalCount: 3,
			wantErr:        false,
		},
		{
			name: "get all Sites by rack level administration flag",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config:                    &SiteConfigFilterInput{Flow: cutil.GetPtr(true)},
				},
				includeRelations: false,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Sites by security group flag",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config:                    &SiteConfigFilterInput{NetworkSecurityGroup: cutil.GetPtr(true)},
				},
				includeRelations: false,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Sites by security group flag via config struct",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config:                    &SiteConfigFilterInput{NetworkSecurityGroup: cutil.GetPtr(true)},
				},
				includeRelations: false,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Sites by combined config",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					Config: &SiteConfigFilterInput{
						NetworkSecurityGroup:             cutil.GetPtr(true),
						NativeNetworking:                 cutil.GetPtr(true),
						MaxNetworkSecurityGroupRuleCount: cutil.GetPtr(100),
					},
				},
				includeRelations: false,
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantErr:        false,
		},
		{
			name: "get all Sites by set of IDs and include relations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   []uuid.UUID{sites[0].ID, sites[1].ID},
				},
				infrastructureProvider: ip,
				includeRelations:       true,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Sites by set of IDs when number of ids is 0",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   []uuid.UUID{},
				},
				includeRelations: false,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantErr:        false,
		},
		{
			name: "get all Sites with limit",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       &ip.Org,
					InfrastructureProviderIDs: nil,
				},
				page: paginator.PageInput{
					Limit: cutil.GetPtr(10),
				},
				includeRelations: false,
			},
			wantCount:      10,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with offset",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       &ip.Org,
					InfrastructureProviderIDs: nil,
				},
				page: paginator.PageInput{
					Offset: cutil.GetPtr(20),
				},
				includeRelations: false,
			},
			wantCount:      5,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites ordered by name",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       &ip.Org,
					InfrastructureProviderIDs: nil,
				},
				page: paginator.PageInput{
					OrderBy: &paginator.OrderBy{Field: "name", Order: paginator.OrderAscending},
				},
				firstEntry:       &sites[24],
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with name search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr("test"),
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with name substring in search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr("est"),
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with description search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr("test-description"),
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with status search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr(SiteStatusPending),
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with empty search query returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr(""),
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with status query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					Statuses:                  []string{SiteStatusPending},
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites with multiple status query values",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					Statuses:                  []string{SiteStatusPending, SiteStatusError},
				},
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
		{
			name: "get all Sites ordered by description",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       &ip.Org,
					InfrastructureProviderIDs: nil,
				},
				page: paginator.PageInput{
					OrderBy: &paginator.OrderBy{Field: "description", Order: paginator.OrderAscending},
				},
				firstEntry:       &sites[24],
				includeRelations: false,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: len(sites),
			wantErr:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			relations := []string{}
			if tt.args.includeRelations {
				relations = append(relations, InfrastructureProviderRelationName)
			}

			got, total, err := ssd.GetAll(tt.args.ctx, nil, tt.args.filter, tt.args.page, relations)
			if (err != nil) != tt.wantErr {
				t.Errorf("SiteSQLDAO.GetAll() error = %v, wantErr = %v", err, tt.wantErr)
				return
			}

			assert.Equal(t, tt.wantCount, len(got))
			assert.Equal(t, tt.wantTotalCount, total)

			if tt.args.includeRelations {
				assert.Equal(t, tt.args.infrastructureProvider.ID, got[0].InfrastructureProvider.ID)
			}

			if tt.args.firstEntry != nil {
				assert.Equal(t, tt.args.firstEntry.Name, got[0].Name)
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSiteSQLDAO_GetCount(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx    context.Context
		filter SiteFilterInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test-org",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// time.Now sets a timezone, but reading back from postgres doesn't load a TZ for zero offset
	expirationTime := db.GetCurTime()

	// Create sites
	sites := []Site{}
	for i := 25; i > 0; i-- {
		site := &Site{
			ID:                          uuid.New(),
			Name:                        fmt.Sprintf("test%v", i),
			Description:                 cutil.GetPtr("test-description"),
			DisplayName:                 cutil.GetPtr(fmt.Sprintf("Test %v", i)),
			Org:                         ip.Org,
			InfrastructureProviderID:    ip.ID,
			SiteControllerVersion:       cutil.GetPtr("1.0.0"),
			SiteAgentVersion:            cutil.GetPtr("1.0.0"),
			RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
			RegistrationTokenExpiration: cutil.GetPtr(expirationTime),
			SerialConsoleHostname:       cutil.GetPtr(fmt.Sprintf("test-serial-console-hostname%v", i)),
			Status:                      SiteStatusPending,
			CreatedBy:                   uuid.New(),
		}

		_, err = dbSession.DB.NewInsert().Model(site).Exec(context.Background())
		if err != nil {
			t.Fatal(err)
		}

		sites = append(sites, *site)
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get count Sites by Infrastructure Provider ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: SiteFilterInput{
					Name:                      nil,
					Org:                       nil,
					InfrastructureProviderIDs: []uuid.UUID{ip.ID},
					SiteIDs:                   nil,
				},
			},
			wantCount:          len(sites),
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by name",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Name:                      cutil.GetPtr("test1"),
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
				},
			},
			wantCount: 1,
			wantErr:   false,
		},
		{
			name: "get count Sites by Org",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       &ip.Org,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get all Sites by set of IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   []uuid.UUID{sites[0].ID, sites[1].ID},
				},
			},
			wantCount: 2,
			wantErr:   false,
		},
		{
			name: "get count Sites by set of IDs when number of ids is 0",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   []uuid.UUID{},
				},
			},
			wantCount: 0,
			wantErr:   false,
		},
		{
			name: "get all Sites with name search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr("test"),
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get count Sites with name substring in search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr("est"),
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get count Sites with description search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr("test-description"),
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get count Sites with status search query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr(SiteStatusPending),
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get count Sites with empty search query returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					SearchQuery:               cutil.GetPtr(""),
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get count Sites with status query",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					Statuses:                  []string{SiteStatusPending},
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
		{
			name: "get count Sites with multiple status query values",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: SiteFilterInput{
					Org:                       nil,
					InfrastructureProviderIDs: nil,
					SiteIDs:                   nil,
					Statuses:                  []string{SiteStatusPending, SiteStatusError},
				},
			},
			wantCount: len(sites),
			wantErr:   false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			count, err := ssd.GetCount(tt.args.ctx, nil, tt.args.filter)
			if (err != nil) != tt.wantErr {
				t.Errorf("SiteSQLDAO.GetAllByInfrastructureProvider() error = %v, wantErr = %v", err, tt.wantErr)
				return
			}

			assert.Equal(t, tt.wantCount, count)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSiteSQLDAO_Create(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	st := &Site{
		ID:                            uuid.New(),
		Name:                          "test",
		DisplayName:                   cutil.GetPtr("Test"),
		Description:                   cutil.GetPtr("Test"),
		Org:                           "test",
		InfrastructureProviderID:      ip.ID,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(db.GetCurTime()),
		IsInfinityEnabled:             false,
		SerialConsoleHostname:         cutil.GetPtr("serialConsoleHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(10),
		SerialConsoleMaxSessionLength: cutil.GetPtr(20),
		Status:                        SiteStatusPending,
		CreatedBy:                     uuid.New(),
		Config:                        &SiteConfig{NativeNetworking: true},
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		ctx                context.Context
		input              SiteCreateInput
		want               *Site
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create Site from params",
			fields: fields{
				dbSession: dbSession,
			},
			ctx: ctx,
			input: SiteCreateInput{
				Name:                          st.Name,
				DisplayName:                   st.DisplayName,
				Description:                   st.Description,
				Org:                           st.Org,
				InfrastructureProviderID:      st.InfrastructureProviderID,
				SiteControllerVersion:         st.SiteControllerVersion,
				SiteAgentVersion:              st.SiteAgentVersion,
				RegistrationToken:             st.RegistrationToken,
				RegistrationTokenExpiration:   st.RegistrationTokenExpiration,
				IsInfinityEnabled:             st.IsInfinityEnabled,
				SerialConsoleHostname:         st.SerialConsoleHostname,
				IsSerialConsoleEnabled:        st.IsSerialConsoleEnabled,
				SerialConsoleIdleTimeout:      st.SerialConsoleIdleTimeout,
				SerialConsoleMaxSessionLength: st.SerialConsoleMaxSessionLength,
				Status:                        st.Status,
				CreatedBy:                     st.CreatedBy,
				Config:                        *st.Config,
			},
			want:               st,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ssd.Create(tt.ctx, nil, tt.input)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.DisplayName, *got.DisplayName)
			assert.Equal(t, *tt.want.Description, *got.Description)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, tt.want.InfrastructureProviderID, got.InfrastructureProviderID)
			assert.Equal(t, *tt.want.SiteControllerVersion, *got.SiteControllerVersion)
			assert.Equal(t, *tt.want.SiteAgentVersion, *got.SiteAgentVersion)
			assert.Equal(t, *tt.want.RegistrationToken, *got.RegistrationToken)
			assert.True(t, got.RegistrationTokenExpiration.Equal(*tt.want.RegistrationTokenExpiration))
			assert.Equal(t, tt.want.IsInfinityEnabled, got.IsInfinityEnabled)
			assert.Equal(t, *tt.want.SerialConsoleHostname, *got.SerialConsoleHostname)
			assert.Equal(t, tt.want.IsSerialConsoleEnabled, got.IsSerialConsoleEnabled)
			assert.Equal(t, *tt.want.SerialConsoleIdleTimeout, *got.SerialConsoleIdleTimeout)
			assert.Equal(t, *tt.want.SerialConsoleMaxSessionLength, *got.SerialConsoleMaxSessionLength)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)
			assert.Equal(t, *tt.want.Config, *got.Config)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSiteSQLDAO_Update(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	st := &Site{
		ID:                          uuid.New(),
		Name:                        "test",
		DisplayName:                 cutil.GetPtr("Test"),
		Description:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		SerialConsoleHostname:       cutil.GetPtr("serialConsoleHostname"),
		IsInfinityEnabled:           false,
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
		Config: &SiteConfig{
			NetworkSecurityGroup: true,
			NativeNetworking:     false,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// Create another site without AgentCertExpiry for additional AgentCertExpiry-specific tests
	st2 := &Site{
		ID:                          uuid.New(),
		Name:                        "test-agent-cert",
		DisplayName:                 cutil.GetPtr("Test Agent Cert"),
		Description:                 cutil.GetPtr("Test Agent Cert Desc"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("abcd-efgh-ijkl-mnop"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		SerialConsoleHostname:       cutil.GetPtr("serialConsoleHostname2"),
		IsInfinityEnabled:           true,
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
		Config: &SiteConfig{
			NativeNetworking: true,
		},
		// No AgentCertExpiry initially
	}

	_, err = dbSession.DB.NewInsert().Model(st2).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// Legacy Sites can have a SQL NULL config because the migration from the
	// legacy capabilities column copied NULL values. Config updates must
	// initialize that row instead of letting NULL absorb the JSONB merge.
	legacyNullConfigSite := &Site{
		ID:                       uuid.New(),
		Name:                     "test-null-config",
		Org:                      "test",
		InfrastructureProviderID: ip.ID,
		IsInfinityEnabled:        true,
		Status:                   SiteStatusPending,
		CreatedBy:                uuid.New(),
	}

	_, err = dbSession.DB.NewInsert().Model(legacyNullConfigSite).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// Current time
	curTime := db.GetCurTime()

	ust := &Site{
		ID:                            st.ID,
		Name:                          "test 2",
		DisplayName:                   cutil.GetPtr("Test 2"),
		Description:                   cutil.GetPtr("Test 2"),
		SiteControllerVersion:         cutil.GetPtr("1.0.1"),
		SiteAgentVersion:              cutil.GetPtr("1.0.1"),
		RegistrationToken:             cutil.GetPtr("9867-6543-2109-8765"),
		RegistrationTokenExpiration:   cutil.GetPtr(time.Now().Add(time.Hour * 24).UTC().Round(time.Microsecond)),
		IsInfinityEnabled:             true,
		SerialConsoleHostname:         st.SerialConsoleHostname,
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(10),
		SerialConsoleMaxSessionLength: cutil.GetPtr(20),
		InventoryReceived:             &curTime,
		Status:                        SiteStatusRegistered,
		Config: &SiteConfig{
			NativeNetworking:     true,
			NetworkSecurityGroup: true,
		},
	}

	agentCertTime1 := time.Now().Add(48 * time.Hour).UTC().Round(time.Microsecond)
	agentCertTime2 := time.Now().Add(72 * time.Hour).UTC().Round(time.Microsecond)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		ctx                context.Context
		input              SiteUpdateInput
		want               *Site
		wantErr            bool
		verifyChildSpanner bool
		verifyAgentCert    bool
		agentCertExpected  *time.Time
		wantVpcSlaac       *bool
	}{
		{
			name: "update only site config from params",
			fields: fields{
				dbSession: dbSession,
			},
			ctx: ctx,
			input: SiteUpdateInput{
				SiteID: ust.ID,
				Config: &SiteConfigUpdateInput{
					NativeNetworking: cutil.GetPtr(false),
				},
			},
			want:               nil,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "update site from params",
			fields: fields{
				dbSession: dbSession,
			},
			ctx: ctx,
			input: SiteUpdateInput{
				SiteID:                        ust.ID,
				Name:                          cutil.GetPtr(ust.Name),
				DisplayName:                   ust.DisplayName,
				Description:                   ust.Description,
				SiteControllerVersion:         ust.SiteControllerVersion,
				SiteAgentVersion:              ust.SiteAgentVersion,
				RegistrationToken:             ust.RegistrationToken,
				RegistrationTokenExpiration:   ust.RegistrationTokenExpiration,
				IsInfinityEnabled:             &ust.IsInfinityEnabled,
				SerialConsoleHostname:         ust.SerialConsoleHostname,
				IsSerialConsoleEnabled:        &ust.IsSerialConsoleEnabled,
				SerialConsoleIdleTimeout:      ust.SerialConsoleIdleTimeout,
				SerialConsoleMaxSessionLength: ust.SerialConsoleMaxSessionLength,
				InventoryReceived:             cutil.GetPtr(curTime),
				Status:                        cutil.GetPtr(ust.Status),
				Config: &SiteConfigUpdateInput{
					NativeNetworking: cutil.GetPtr(true),
				},
			},
			want:               ust,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "update site by setting AgentCertExpiry",
			fields: fields{
				dbSession: dbSession,
			},
			ctx: ctx,
			input: SiteUpdateInput{
				SiteID:          st2.ID,
				AgentCertExpiry: &agentCertTime1,
			},
			want:               st2,
			wantErr:            false,
			verifyChildSpanner: true,
			verifyAgentCert:    true,
			agentCertExpected:  &agentCertTime1,
		},
		{
			name: "update site by changing AgentCertExpiry",
			fields: fields{
				dbSession: dbSession,
			},
			ctx: ctx,
			input: SiteUpdateInput{
				SiteID:          st2.ID,
				AgentCertExpiry: &agentCertTime2,
			},
			want:               st2,
			wantErr:            false,
			verifyChildSpanner: true,
			verifyAgentCert:    true,
			agentCertExpected:  &agentCertTime2,
		},
		{
			name: "update VPC SLAAC config when stored config is SQL NULL",
			fields: fields{
				dbSession: dbSession,
			},
			ctx: ctx,
			input: SiteUpdateInput{
				SiteID: legacyNullConfigSite.ID,
				Config: &SiteConfigUpdateInput{
					VpcSlaac: cutil.GetPtr(true),
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
			wantVpcSlaac:       cutil.GetPtr(true),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ssd.Update(tt.ctx, nil, tt.input)

			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			assert.NoError(t, err)
			assert.NotNil(t, got)
			if tt.wantVpcSlaac != nil && assert.NotNil(t, got.Config) {
				assert.Equal(t, *tt.wantVpcSlaac, got.Config.VpcSlaac)
			}

			if tt.want == ust {
				assert.Equal(t, tt.want.Name, got.Name)
				assert.Equal(t, *tt.want.DisplayName, *got.DisplayName)
				assert.Equal(t, *tt.want.Description, *got.Description)
				assert.Equal(t, *tt.want.SiteControllerVersion, *got.SiteControllerVersion)
				assert.Equal(t, *tt.want.SiteAgentVersion, *got.SiteAgentVersion)
				assert.Equal(t, *tt.want.RegistrationToken, *got.RegistrationToken)
				assert.True(t, got.RegistrationTokenExpiration.Equal(*tt.want.RegistrationTokenExpiration))
				assert.Equal(t, tt.want.IsInfinityEnabled, got.IsInfinityEnabled)
				assert.Equal(t, *tt.want.SerialConsoleHostname, *got.SerialConsoleHostname)
				assert.Equal(t, tt.want.IsSerialConsoleEnabled, got.IsSerialConsoleEnabled)
				assert.Equal(t, *tt.want.SerialConsoleIdleTimeout, *got.SerialConsoleIdleTimeout)
				assert.Equal(t, *tt.want.SerialConsoleMaxSessionLength, *got.SerialConsoleMaxSessionLength)
				assert.True(t, got.InventoryReceived.Equal(*tt.want.InventoryReceived))
				assert.Equal(t, tt.want.Status, got.Status)
				assert.Equal(t, *tt.want.Config, *got.Config)
			}

			// Verify AgentCertExpiry
			if tt.verifyAgentCert && tt.agentCertExpected != nil {
				assert.NotNil(t, got.AgentCertExpiry)
				assert.True(t, got.AgentCertExpiry.Equal(*tt.agentCertExpected), "AgentCertExpiry did not match expected value")
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(tt.ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := tt.ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSiteSQLDAO_Delete(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// time.Now sets a timezone, but reading back from postgres doesn't load a TZ for zero offset
	expirationTime := db.GetCurTime()

	// Create site
	st := &Site{
		ID:                          uuid.New(),
		Name:                        "test",
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(expirationTime),
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}

	_, err = dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "delete site by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  st.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			if err := ssd.Delete(tt.args.ctx, nil, tt.args.id); (err != nil) != tt.wantErr {
				t.Errorf("SiteSQLDAO.DeleteByID() error = %v, wantErr %v", err, tt.wantErr)
			}

			// Check that the site was deleted
			dst := &Site{}
			err := dbSession.DB.NewSelect().Model(dst).WhereDeleted().Where("id = ?", st.ID).Scan(context.Background())
			if err != nil {
				t.Fatal(err)
			}

			if dst.Deleted == nil {
				t.Errorf("Failed to soft-delete InfrastructureProvider")
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSiteSQLDAO_ContactLocation(t *testing.T) {
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	tests := []struct {
		name        string
		want        Site
		create      bool
		update      bool
		updateInput SiteUpdateInput
	}{
		{
			name:   "create no location no contact",
			want:   buildSite("Test1", ip.ID, nil, nil),
			create: true,
		},
		{
			name:   "create with location",
			want:   buildSite("Test2", ip.ID, &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}, nil),
			create: true,
		},
		{
			name:   "create with contact",
			want:   buildSite("Test3", ip.ID, nil, &SiteContact{Email: "test@nvidia.com"}),
			create: true,
		},
		{
			name:   "create with location and contact",
			want:   buildSite("Test4", ip.ID, &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}, &SiteContact{Email: "test@nvidia.com"}),
			create: true,
		},
		{
			name:        "update no location to location",
			want:        buildSite("Test5", ip.ID, nil, nil),
			update:      true,
			updateInput: SiteUpdateInput{Location: &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}},
		},
		{
			name:        "update no contact to contact",
			want:        buildSite("Test6", ip.ID, nil, nil),
			update:      true,
			updateInput: SiteUpdateInput{Contact: &SiteContact{Email: "test@nvidia.com"}},
		},
		{
			name:        "update no location and contact to location and contact",
			want:        buildSite("Test7", ip.ID, nil, nil),
			update:      true,
			updateInput: SiteUpdateInput{Location: &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}, Contact: &SiteContact{Email: "test@nvidia.com"}},
		},
		{
			name:        "update location city state",
			want:        buildSite("Test8", ip.ID, &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}, nil),
			update:      true,
			updateInput: SiteUpdateInput{Location: &SiteLocation{City: "San Jose", State: "CA", Country: "USA"}},
		},
		{
			name:        "update contact email",
			want:        buildSite("Test9", ip.ID, nil, &SiteContact{Email: "test@nvidia.com"}),
			update:      true,
			updateInput: SiteUpdateInput{Contact: &SiteContact{Email: "test@amazon.com"}},
		},
		{
			name:        "update location city state and contact email",
			want:        buildSite("Test9", ip.ID, &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}, &SiteContact{Email: "test@nvidia.com"}),
			update:      true,
			updateInput: SiteUpdateInput{Location: &SiteLocation{City: "San Jose", State: "CA", Country: "USA"}, Contact: &SiteContact{Email: "test@amazon.com"}},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SiteSQLDAO{
				dbSession: dbSession,
			}
			// OTEL Spanner configuration
			_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

			want := tt.want
			if tt.create {
				createInput := buildSiteCreateInput(want)
				got, err := ssd.Create(ctx, nil, createInput)
				assert.NoError(t, err)
				assert.NotNil(t, got)
				validateSite(t, *got, want)
			} else if tt.update {
				// first create
				createInput := buildSiteCreateInput(want)
				got, err := ssd.Create(ctx, nil, createInput)
				assert.NoError(t, err)
				assert.NotNil(t, got)
				validateSite(t, *got, want)
				// update
				want.Location = tt.updateInput.Location
				want.Contact = tt.updateInput.Contact
				updateInput := tt.updateInput
				updateInput.SiteID = got.ID
				got, err = ssd.Update(ctx, nil, updateInput)
				assert.NoError(t, err)
				assert.NotNil(t, got)
				validateSite(t, *got, want)
			}
		})
	}
}

func TestSiteSQLDAO_QueryByContactLocation(t *testing.T) {
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        "test",
		DisplayName: cutil.GetPtr("Test"),
		Org:         "test",
	}

	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	ssd := SiteSQLDAO{
		dbSession: dbSession,
	}
	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	// create sites with different locations and contacts
	var createdSites []*Site
	for i := 0; i < 30; i++ {
		var location *SiteLocation
		var contact *SiteContact
		if i%2 == 0 {
			location = &SiteLocation{City: "Seattle", State: "WA", Country: "USA"}
			contact = &SiteContact{Email: "test@nvidia.com"}
		} else if i%3 == 0 {
			location = &SiteLocation{City: "San Jose", State: "CA", Country: "USA"}
			contact = &SiteContact{Email: "test@amazon.com"}
		}
		site := buildSite(fmt.Sprintf("Test%d", i), ip.ID, location, contact)
		createInput := buildSiteCreateInput(site)
		got, err := ssd.Create(ctx, nil, createInput)
		assert.NoError(t, err)
		assert.NotNil(t, got)
		validateSite(t, *got, site)
		createdSites = append(createdSites, got)
	}

	// query by location city
	sites, total, err := ssd.GetAll(ctx, nil, SiteFilterInput{SearchQuery: cutil.GetPtr("San Jose")}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 5, len(sites))
	assert.Equal(t, 5, total)

	// query by location city or state
	sites, total, err = ssd.GetAll(ctx, nil, SiteFilterInput{SearchQuery: cutil.GetPtr("San Jose CA")}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 5, len(sites))
	assert.Equal(t, 5, total)

	// query by location city or country
	sites, total, err = ssd.GetAll(ctx, nil, SiteFilterInput{SearchQuery: cutil.GetPtr("San Jose USA")}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 20, len(sites))
	assert.Equal(t, 20, total)

	// query by email
	sites, total, err = ssd.GetAll(ctx, nil, SiteFilterInput{SearchQuery: cutil.GetPtr("test@nvidia.com")}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 15, len(sites))
	assert.Equal(t, 15, total)

	// test sort by location
	sites, total, err = ssd.GetAll(ctx, nil, SiteFilterInput{}, paginator.PageInput{OrderBy: &paginator.OrderBy{Field: "location", Order: "ASC"}}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 20, len(sites))
	assert.Equal(t, 30, total)
	// make sure correct site is returned first
	assert.Equal(t, createdSites[3].ID, sites[0].ID)

	// test sort by contact
	sites, total, err = ssd.GetAll(ctx, nil, SiteFilterInput{}, paginator.PageInput{OrderBy: &paginator.OrderBy{Field: "contact", Order: "DESC"}}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 20, len(sites))
	assert.Equal(t, 30, total)
	// make sure correct site is returned first
	assert.Equal(t, createdSites[1].ID, sites[0].ID)
}

func buildSite(name string, ipID uuid.UUID, location *SiteLocation, contact *SiteContact) Site {
	return Site{
		Name:                          name,
		DisplayName:                   &name,
		Description:                   cutil.GetPtr("Test Site"),
		Org:                           "test",
		InfrastructureProviderID:      ipID,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(db.GetCurTime()),
		IsInfinityEnabled:             false,
		SerialConsoleHostname:         cutil.GetPtr("serialConsoleHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(10),
		SerialConsoleMaxSessionLength: cutil.GetPtr(20),
		Status:                        SiteStatusPending,
		CreatedBy:                     uuid.New(),
		Location:                      location,
		Contact:                       contact,
	}
}

func buildSiteCreateInput(site Site) SiteCreateInput {
	return SiteCreateInput{
		Name:                          site.Name,
		DisplayName:                   site.DisplayName,
		Description:                   site.Description,
		Org:                           site.Org,
		InfrastructureProviderID:      site.InfrastructureProviderID,
		SiteControllerVersion:         site.SiteControllerVersion,
		SiteAgentVersion:              site.SiteAgentVersion,
		RegistrationToken:             site.RegistrationToken,
		RegistrationTokenExpiration:   site.RegistrationTokenExpiration,
		IsInfinityEnabled:             site.IsInfinityEnabled,
		SerialConsoleHostname:         site.SerialConsoleHostname,
		IsSerialConsoleEnabled:        site.IsSerialConsoleEnabled,
		SerialConsoleIdleTimeout:      site.SerialConsoleIdleTimeout,
		SerialConsoleMaxSessionLength: site.SerialConsoleMaxSessionLength,
		Status:                        site.Status,
		CreatedBy:                     site.CreatedBy,
		Location:                      site.Location,
		Contact:                       site.Contact,
	}
}

func validateSite(t *testing.T, got Site, want Site) {
	assert.Equal(t, want.Name, got.Name)
	assert.Equal(t, *want.DisplayName, *got.DisplayName)
	assert.Equal(t, *want.Description, *got.Description)
	assert.Equal(t, want.Org, got.Org)
	assert.Equal(t, want.InfrastructureProviderID, got.InfrastructureProviderID)
	assert.Equal(t, *want.SiteControllerVersion, *got.SiteControllerVersion)
	assert.Equal(t, *want.SiteAgentVersion, *got.SiteAgentVersion)
	assert.Equal(t, *want.RegistrationToken, *got.RegistrationToken)
	assert.True(t, got.RegistrationTokenExpiration.Equal(*want.RegistrationTokenExpiration))
	assert.Equal(t, want.IsInfinityEnabled, got.IsInfinityEnabled)
	assert.Equal(t, *want.SerialConsoleHostname, *got.SerialConsoleHostname)
	assert.Equal(t, want.IsSerialConsoleEnabled, got.IsSerialConsoleEnabled)
	assert.Equal(t, *want.SerialConsoleIdleTimeout, *got.SerialConsoleIdleTimeout)
	assert.Equal(t, *want.SerialConsoleMaxSessionLength, *got.SerialConsoleMaxSessionLength)
	assert.Equal(t, want.Status, got.Status)
	assert.Equal(t, want.CreatedBy, got.CreatedBy)
	assert.Equal(t, want.Location, got.Location)
	assert.Equal(t, want.Contact, got.Contact)
}

func TestSite_IsTimeWithinStaleInventoryThreshold(t *testing.T) {
	reportedOneMinute := &Site{InventoryIntervalSeconds: cutil.GetPtr(60)}

	tests := []struct {
		name       string
		site       *Site
		actionTime time.Time
		want       bool
	}{
		{
			name:       "a change just now is too recent to act on",
			site:       &Site{},
			actionTime: time.Now(),
			want:       true,
		},
		{
			name:       "a change older than the fallback threshold is safe to act on",
			site:       &Site{},
			actionTime: time.Now().Add(-(cutil.DefaultInventoryReceiptInterval + cutil.StaleInventoryBuffer + time.Second)),
			want:       false,
		},
		{
			// The same age that clears the fallback is still too recent for a slower Site.
			name:       "follows a reported interval longer than the fallback",
			site:       &Site{InventoryIntervalSeconds: cutil.GetPtr(300)},
			actionTime: time.Now().Add(-(cutil.DefaultInventoryReceiptInterval + cutil.StaleInventoryBuffer + time.Second)),
			want:       true,
		},
		{
			// The same age is stale against the fallback and safe against a faster Site, which
			// is the whole point of following the reported interval.
			name:       "an age between the two intervals depends on the reported one",
			site:       reportedOneMinute,
			actionTime: time.Now().Add(-2 * time.Minute),
			want:       false,
		},
		{
			name:       "the same age is still too recent against the fallback",
			site:       &Site{},
			actionTime: time.Now().Add(-2 * time.Minute),
			want:       true,
		},
		{
			// The buffer keeps the check off the exact interval, where clock skew between the
			// Site and Cloud would decide the outcome.
			name:       "a change inside the buffer past the interval is still too recent",
			site:       reportedOneMinute,
			actionTime: time.Now().Add(-(time.Minute + cutil.StaleInventoryBuffer/2)),
			want:       true,
		},
		{
			// A stored zero or negative would otherwise collapse the threshold to the buffer
			// alone and let inventory act on data it should treat as newer.
			name:       "falls back on a zero reported interval",
			site:       &Site{InventoryIntervalSeconds: cutil.GetPtr(0)},
			actionTime: time.Now().Add(-(cutil.StaleInventoryBuffer + time.Second)),
			want:       true,
		},
		{
			name:       "falls back on a negative reported interval",
			site:       &Site{InventoryIntervalSeconds: cutil.GetPtr(-30)},
			actionTime: time.Now().Add(-(cutil.StaleInventoryBuffer + time.Second)),
			want:       true,
		},
		{
			name:       "falls back on a nil Site",
			site:       nil,
			actionTime: time.Now().Add(-(cutil.StaleInventoryBuffer + time.Second)),
			want:       true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.site.IsTimeWithinStaleInventoryThreshold(tt.actionTime))
		})
	}
}
