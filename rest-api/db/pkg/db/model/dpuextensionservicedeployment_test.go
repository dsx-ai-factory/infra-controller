// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestDpuExtensionServiceDeploymentSQLDAO_GetByID(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	it := testBuildInstanceType(t, dbSession, nil, "test-instance-type", ip.ID, st.ID, tnu.ID)
	vpc := TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, VpcStatusPending, ipu, nil)

	// Create a minimal Instance directly
	instance := &Instance{
		ID:                       uuid.New(),
		Name:                     "test-instance",
		SiteID:                   st.ID,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(instance).Exec(context.Background())
	require.NoError(t, err)

	// Create DpuExtensionService
	des := testBuildDpuExtensionService(t, dbSession, nil, "test-dpu-service", DpuExtensionServiceServiceTypeKubernetesPod, st.ID, tn.ID, cutil.GetPtr("V1-T1761856992374052"), DpuExtensionServiceStatusReady, tnu.ID)

	// Create DpuExtensionServiceDeployment
	desd := testBuildDpuExtensionServiceDeployment(t, dbSession, nil, st.ID, tn.ID, instance.ID, des.ID, *des.Version, DpuExtensionServiceDeploymentStatusRunning, tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *DpuExtensionServiceDeployment
		wantErr            error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			name: "get DpuExtensionServiceDeployment by ID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  desd.ID,
			},
			want:               desd,
			wantErr:            nil,
			paramRelations:     []string{SiteRelationName, TenantRelationName, InstanceRelationName, DpuExtensionServiceRelationName},
			verifyChildSpanner: true,
		},
		{
			name: "get DpuExtensionServiceDeployment by non-existent ID returns error",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				id:  uuid.New(),
			},
			want:    nil,
			wantErr: db.ErrDoesNotExist,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			desdsd := DpuExtensionServiceDeploymentSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			got, err := desdsd.GetByID(tt.args.ctx, nil, tt.args.id, tt.paramRelations)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if err == nil {
				if len(tt.paramRelations) > 0 {
					assert.NotNil(t, got.Site)
					assert.NotNil(t, got.Tenant)
					assert.NotNil(t, got.Instance)
					assert.NotNil(t, got.DpuExtensionService)
				}
				assert.EqualValues(t, tt.want.ID, got.ID)
				assert.EqualValues(t, tt.want.SiteID, got.SiteID)
				assert.EqualValues(t, tt.want.TenantID, got.TenantID)
				assert.EqualValues(t, tt.want.InstanceID, got.InstanceID)
				assert.EqualValues(t, tt.want.DpuExtensionServiceID, got.DpuExtensionServiceID)
				assert.EqualValues(t, tt.want.Version, got.Version)
				assert.EqualValues(t, tt.want.Status, got.Status)
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDpuExtensionServiceDeploymentSQLDAO_GetAll(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx                            context.Context
		dpuExtensionServiceDeployments []uuid.UUID
		siteIDs                        []uuid.UUID
		tenantIDs                      []uuid.UUID
		instanceIDs                    []uuid.UUID
		dpuExtensionServiceIDs         []uuid.UUID
		versions                       []string
		statuses                       []string
		searchQuery                    *string
		offset                         *int
		limit                          *int
		orderBy                        *paginator.OrderBy
		paramRelations                 []string
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	it := testBuildInstanceType(t, dbSession, nil, "test-instance-type", ip.ID, st.ID, tnu.ID)
	vpc := TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, VpcStatusPending, ipu, nil)

	// Create Instances
	instance1 := &Instance{
		ID:                       uuid.New(),
		Name:                     "test-instance-1",
		SiteID:                   st.ID,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(instance1).Exec(context.Background())
	require.NoError(t, err)

	instance2 := &Instance{
		ID:                       uuid.New(),
		Name:                     "test-instance-2",
		SiteID:                   st.ID,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	}
	_, err = dbSession.DB.NewInsert().Model(instance2).Exec(context.Background())
	require.NoError(t, err)

	// Create DpuExtensionServices
	des1 := testBuildDpuExtensionService(t, dbSession, nil, "test-dpu-service-1", DpuExtensionServiceServiceTypeKubernetesPod, st.ID, tn.ID, cutil.GetPtr("V1-T1761856992374052"), DpuExtensionServiceStatusReady, tnu.ID)
	des2 := testBuildDpuExtensionService(t, dbSession, nil, "test-dpu-service-2", DpuExtensionServiceServiceTypeKubernetesPod, st.ID, tn.ID, cutil.GetPtr("V1-T1761856992374053"), DpuExtensionServiceStatusReady, tnu.ID)

	// Create DpuExtensionServiceDeployments
	desd1 := testBuildDpuExtensionServiceDeployment(t, dbSession, nil, st.ID, tn.ID, instance1.ID, des1.ID, *des1.Version, DpuExtensionServiceDeploymentStatusPending, tnu.ID)
	_ = testBuildDpuExtensionServiceDeployment(t, dbSession, nil, st.ID, tn.ID, instance1.ID, des1.ID, *des1.Version, DpuExtensionServiceDeploymentStatusRunning, tnu.ID)
	desd3 := testBuildDpuExtensionServiceDeployment(t, dbSession, nil, st.ID, tn.ID, instance2.ID, des2.ID, *des2.Version, DpuExtensionServiceDeploymentStatusError, tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotal          int
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get all DpuExtensionServiceDeployments returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
			},
			wantCount:          3,
			wantTotal:          3,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get DpuExtensionServiceDeployments by IDs returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                            ctx,
				dpuExtensionServiceDeployments: []uuid.UUID{desd1.ID, desd3.ID},
			},
			wantCount: 2,
			wantTotal: 2,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments by SiteID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:     ctx,
				siteIDs: []uuid.UUID{st.ID},
			},
			wantCount: 3,
			wantTotal: 3,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments by TenantID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       ctx,
				tenantIDs: []uuid.UUID{tn.ID},
			},
			wantCount: 3,
			wantTotal: 3,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments by InstanceID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         ctx,
				instanceIDs: []uuid.UUID{instance1.ID},
			},
			wantCount: 2,
			wantTotal: 2,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments by DpuExtensionServiceID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                    ctx,
				dpuExtensionServiceIDs: []uuid.UUID{des1.ID},
			},
			wantCount: 2,
			wantTotal: 2,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments by Version returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      ctx,
				versions: []string{"V1-T1761856992374052"},
			},
			wantCount: 2,
			wantTotal: 2,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments by Status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      ctx,
				statuses: []string{DpuExtensionServiceDeploymentStatusPending, DpuExtensionServiceDeploymentStatusRunning},
			},
			wantCount: 2,
			wantTotal: 2,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments with search query returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         ctx,
				searchQuery: cutil.GetPtr("Pending"),
			},
			wantCount: 1,
			wantTotal: 1,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments with limit returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    ctx,
				limit:  cutil.GetPtr(2),
				offset: cutil.GetPtr(0),
			},
			wantCount: 2,
			wantTotal: 3,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments with offset returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    ctx,
				offset: cutil.GetPtr(1),
			},
			wantCount: 2,
			wantTotal: 3,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments with orderBy returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				orderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderDescending,
				},
			},
			wantCount: 3,
			wantTotal: 3,
			wantErr:   false,
		},
		{
			name: "get DpuExtensionServiceDeployments with relations returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            ctx,
				paramRelations: []string{SiteRelationName, TenantRelationName, InstanceRelationName, DpuExtensionServiceRelationName},
			},
			wantCount: 3,
			wantTotal: 3,
			wantErr:   false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			desdsd := DpuExtensionServiceDeploymentSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			filter := DpuExtensionServiceDeploymentFilterInput{
				DpuExtensionServiceDeploymentIDs: tt.args.dpuExtensionServiceDeployments,
				SiteIDs:                          tt.args.siteIDs,
				TenantIDs:                        tt.args.tenantIDs,
				InstanceIDs:                      tt.args.instanceIDs,
				DpuExtensionServiceIDs:           tt.args.dpuExtensionServiceIDs,
				Versions:                         tt.args.versions,
				Statuses:                         tt.args.statuses,
				SearchQuery:                      tt.args.searchQuery,
			}

			page := paginator.PageInput{
				Offset:  tt.args.offset,
				Limit:   tt.args.limit,
				OrderBy: tt.args.orderBy,
			}

			got, total, err := desdsd.GetAll(tt.args.ctx, nil, filter, page, tt.args.paramRelations)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, tt.wantCount, len(got))
			assert.Equal(t, tt.wantTotal, total)

			if len(tt.args.paramRelations) > 0 {
				for _, desd := range got {
					assert.NotNil(t, desd.Site)
					assert.NotNil(t, desd.Tenant)
					assert.NotNil(t, desd.Instance)
					assert.NotNil(t, desd.DpuExtensionService)
				}
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDpuExtensionServiceDeploymentSQLDAO_Create(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx   context.Context
		input DpuExtensionServiceDeploymentCreateInput
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	it := testBuildInstanceType(t, dbSession, nil, "test-instance-type", ip.ID, st.ID, tnu.ID)
	vpc := TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, VpcStatusPending, ipu, nil)

	// Create Instance
	instance := &Instance{
		ID:                       uuid.New(),
		Name:                     "test-instance",
		SiteID:                   st.ID,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(instance).Exec(context.Background())
	require.NoError(t, err)

	// Create DpuExtensionService
	des := testBuildDpuExtensionService(t, dbSession, nil, "test-dpu-service", DpuExtensionServiceServiceTypeKubernetesPod, st.ID, tn.ID, cutil.GetPtr("V1-T1761856992374052"), DpuExtensionServiceStatusReady, tnu.ID)

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
			name: "create DpuExtensionServiceDeployment returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: DpuExtensionServiceDeploymentCreateInput{
					DpuExtensionServiceDeploymentID: cutil.GetPtr(uuid.New()),
					SiteID:                          st.ID,
					TenantID:                        tn.ID,
					InstanceID:                      instance.ID,
					DpuExtensionServiceID:           des.ID,
					Version:                         "V1-T1761856992374052",
					Status:                          DpuExtensionServiceDeploymentStatusPending,
					CreatedBy:                       tnu.ID,
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "create DpuExtensionServiceDeployment with auto-generated ID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: DpuExtensionServiceDeploymentCreateInput{
					DpuExtensionServiceDeploymentID: nil,
					SiteID:                          st.ID,
					TenantID:                        tn.ID,
					InstanceID:                      instance.ID,
					DpuExtensionServiceID:           des.ID,
					Version:                         "V1-T1761856992374053",
					Status:                          DpuExtensionServiceDeploymentStatusRunning,
					CreatedBy:                       tnu.ID,
				},
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			desdsd := DpuExtensionServiceDeploymentSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			got, err := desdsd.Create(tt.args.ctx, nil, tt.args.input)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tt.args.input.SiteID, got.SiteID)
			assert.Equal(t, tt.args.input.TenantID, got.TenantID)
			assert.Equal(t, tt.args.input.InstanceID, got.InstanceID)
			assert.Equal(t, tt.args.input.DpuExtensionServiceID, got.DpuExtensionServiceID)
			assert.Equal(t, tt.args.input.Version, got.Version)
			assert.Equal(t, tt.args.input.Status, got.Status)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDpuExtensionServiceDeploymentSQLDAO_Update(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx   context.Context
		input DpuExtensionServiceDeploymentUpdateInput
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	it := testBuildInstanceType(t, dbSession, nil, "test-instance-type", ip.ID, st.ID, tnu.ID)
	vpc := TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, VpcStatusPending, ipu, nil)

	// Create Instance
	instance := &Instance{
		ID:                       uuid.New(),
		Name:                     "test-instance",
		SiteID:                   st.ID,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(instance).Exec(context.Background())
	require.NoError(t, err)

	// Create DpuExtensionService
	des := testBuildDpuExtensionService(t, dbSession, nil, "test-dpu-service", DpuExtensionServiceServiceTypeKubernetesPod, st.ID, tn.ID, cutil.GetPtr("V1-T1761856992374052"), DpuExtensionServiceStatusReady, tnu.ID)

	// Create DpuExtensionServiceDeployment to update
	desd := testBuildDpuExtensionServiceDeployment(t, dbSession, nil, st.ID, tn.ID, instance.ID, des.ID, *des.Version, DpuExtensionServiceDeploymentStatusPending, tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	newStatus := DpuExtensionServiceDeploymentStatusRunning

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update DpuExtensionServiceDeployment status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: DpuExtensionServiceDeploymentUpdateInput{
					DpuExtensionServiceDeploymentID: desd.ID,
					Status:                          &newStatus,
				},
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			desdsd := DpuExtensionServiceDeploymentSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			got, err := desdsd.Update(tt.args.ctx, nil, tt.args.input)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
			assert.NotNil(t, got)

			if tt.args.input.Status != nil {
				assert.Equal(t, *tt.args.input.Status, got.Status)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDpuExtensionServiceDeploymentSQLDAO_Delete(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	it := testBuildInstanceType(t, dbSession, nil, "test-instance-type", ip.ID, st.ID, tnu.ID)
	vpc := TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, VpcStatusPending, ipu, nil)

	// Create Instance
	instance := &Instance{
		ID:                       uuid.New(),
		Name:                     "test-instance",
		SiteID:                   st.ID,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(instance).Exec(context.Background())
	require.NoError(t, err)

	// Create DpuExtensionService
	des := testBuildDpuExtensionService(t, dbSession, nil, "test-dpu-service", DpuExtensionServiceServiceTypeKubernetesPod, st.ID, tn.ID, cutil.GetPtr("V1-T1761856992374052"), DpuExtensionServiceStatusReady, tnu.ID)

	// Create DpuExtensionServiceDeployment to delete
	desd := testBuildDpuExtensionServiceDeployment(t, dbSession, nil, st.ID, tn.ID, instance.ID, des.ID, *des.Version, DpuExtensionServiceDeploymentStatusRunning, tnu.ID)

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
			name: "delete DpuExtensionServiceDeployment returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  desd.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "delete non-existent DpuExtensionServiceDeployment returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  uuid.New(),
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			desdsd := DpuExtensionServiceDeploymentSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			err := desdsd.Delete(tt.args.ctx, nil, tt.args.id)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)

			// Verify deletion
			_, err = desdsd.GetByID(tt.args.ctx, nil, tt.args.id, nil)
			assert.ErrorAs(t, err, &db.ErrDoesNotExist)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDpuExtensionServiceDeploymentSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	TestSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	instance1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			TenantID:                 tn.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   st.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	machine2 := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest2"))
	instance2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			TenantID:                 tn.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   st.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine2.ID,
			Hostname:                 cutil.GetPtr("test2.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	service := testBuildDpuExtensionService(t, dbSession, nil, "test-service", "helm", st.ID, tn.ID, cutil.GetPtr("1.0.0"), DpuExtensionServiceStatusReady, tnu.ID)

	desdsd := NewDpuExtensionServiceDeploymentDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []DpuExtensionServiceDeploymentCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three deployments",
			inputs: []DpuExtensionServiceDeploymentCreateInput{
				{
					SiteID:                st.ID,
					TenantID:              tn.ID,
					InstanceID:            instance1.ID,
					DpuExtensionServiceID: service.ID,
					Version:               "1.0.0",
					Status:                DpuExtensionServiceDeploymentStatusPending,
					CreatedBy:             tnu.ID,
				},
				{
					SiteID:                st.ID,
					TenantID:              tn.ID,
					InstanceID:            instance2.ID,
					DpuExtensionServiceID: service.ID,
					Version:               "1.1.0",
					Status:                DpuExtensionServiceDeploymentStatusRunning,
					CreatedBy:             tnu.ID,
				},
				{
					SiteID:                st.ID,
					TenantID:              tn.ID,
					InstanceID:            instance1.ID,
					DpuExtensionServiceID: service.ID,
					Version:               "2.0.0",
					Status:                DpuExtensionServiceDeploymentStatusPending,
					CreatedBy:             tnu.ID,
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []DpuExtensionServiceDeploymentCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := desdsd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify results are returned in the same order as inputs
				for i, desd := range got {
					assert.NotEqual(t, uuid.Nil, desd.ID)
					assert.Equal(t, tc.inputs[i].Version, desd.Version, "result order should match input order")
					assert.Equal(t, tc.inputs[i].Status, desd.Status)
					assert.NotZero(t, desd.Created)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDpuExtensionServiceDeploymentSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	desdsd := NewDpuExtensionServiceDeploymentDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]DpuExtensionServiceDeploymentCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = DpuExtensionServiceDeploymentCreateInput{
			SiteID:                uuid.New(),
			TenantID:              uuid.New(),
			InstanceID:            uuid.New(),
			DpuExtensionServiceID: uuid.New(),
			Version:               "1.0.0",
			Status:                DpuExtensionServiceDeploymentStatusPending,
			CreatedBy:             uuid.New(),
		}
	}

	_, err := desdsd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}
