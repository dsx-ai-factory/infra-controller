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
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestNVLinkInterfaceSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
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

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	// Create necessary objects for instance
	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	i1, err := isd.Create(
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
	assert.NotNil(t, i1)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)
	nvli := testBuildNVLinkInterface(t, dbSession, nil, st.ID, i1.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 0, nil, cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *NVLinkInterface
		wantErr            error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			name: "get NVLinkInterface by ID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  nvli.ID,
			},
			want:               nvli,
			wantErr:            nil,
			paramRelations:     []string{SiteRelationName, NVLinkLogicalPartitionRelationName},
			verifyChildSpanner: true,
		},
		{
			name: "get NVLinkInterface by non-existent ID returns error",
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
			nvlisd := NVLinkInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			got, err := nvlisd.GetByID(tt.args.ctx, nil, tt.args.id, tt.paramRelations)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if err == nil {
				if len(tt.paramRelations) > 0 {
					assert.NotNil(t, got.Site)
					assert.NotNil(t, got.NVLinkLogicalPartition)
				}
				assert.EqualValues(t, tt.want.ID, got.ID)
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterface_GetAll(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx                       context.Context
		ids                       []uuid.UUID
		siteIDs                   []uuid.UUID
		instanceIDs               []uuid.UUID
		nvlinkLogicalPartitionIDs []uuid.UUID
		searchQuery               *string
		statuses                  []string
		deviceInstances           []int
		offset                    *int
		limit                     *int
		orderBy                   *paginator.OrderBy
		paramRelations            []string
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))

	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu1 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("janed@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn1 := testBuildTenant(t, dbSession, nil, "test-tenant-1", "test-tenant-org-1", tnu1.ID)

	tnu2 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jimd@test.com"), cutil.GetPtr("Jim"), cutil.GetPtr("Doe"))
	tn2 := testBuildTenant(t, dbSession, nil, "test-tenant-2", "test-tenant-org-2", tnu2.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)
	st2 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-2", "Test Site 2", ip.Org, ipu.ID)

	// Create necessary objects for instance
	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn1, "testVpc")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, st2, tn1, "testVpc2")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	machine2 := testMachineBuildMachine(t, dbSession, ip.ID, st2.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest2"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn1, st, "testAllocation")
	allocation2 := testInstanceBuildAllocation(t, dbSession, ip, tn2, st2, "testAllocation2")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	_ = testBuildAllocationConstraint(t, dbSession, allocation2, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			TenantID:                 tn1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   st.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu1.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)
	i2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			TenantID:                 tn2.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   st2.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc2.ID,
			MachineID:                &machine2.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu2.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i2)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn1.Org, tn1.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu1.ID)
	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition2", nil, tn2.Org, tn2.ID, st2.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu2.ID)

	totalCount := 30
	nvlinkInterfaces := []NVLinkInterface{}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	for i := 0; i < totalCount; i++ {
		var nvli *NVLinkInterface
		var tn *Tenant

		if i%2 == 0 {
			tn = tn1
		} else {
			tn = tn2
		}

		if i%2 == 0 {
			nvli = testBuildNVLinkInterface(t, dbSession, nil, st.ID, i1.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), i, cutil.GetPtr("guid"), cutil.GetPtr(NVLinkInterfaceStatusReady), tn.ID)
		} else {
			nvli = testBuildNVLinkInterface(t, dbSession, nil, st2.ID, i2.ID, nvllp2.ID, nil, cutil.GetPtr("Nvidia GB200"), i, cutil.GetPtr("guid"), cutil.GetPtr(NVLinkInterfaceStatusPending), tn.ID)
		}

		nvlinkInterfaces = append(nvlinkInterfaces, *nvli)
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		wantFirstEntry     *NVLinkInterface
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get all with no filters returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all  with relation returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            context.Background(),
				paramRelations: []string{InstanceRelationName, SiteRelationName, NVLinkLogicalPartitionRelationName},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all with Site ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:     context.Background(),
				siteIDs: []uuid.UUID{st.ID},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all with Site ID filter with multiple values returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:     context.Background(),
				siteIDs: []uuid.UUID{st.ID, st2.ID},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all with Instance ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				instanceIDs: []uuid.UUID{i1.ID},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all with NVLink Logical Partition ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                       context.Background(),
				nvlinkLogicalPartitionIDs: []uuid.UUID{nvllp.ID},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all with Device Instance filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:             context.Background(),
				deviceInstances: []int{1},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all with limit returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:     context.Background(),
				siteIDs: []uuid.UUID{st.ID},
				limit:   cutil.GetPtr(10),
			},
			wantCount:      10,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all with offset returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:     context.Background(),
				siteIDs: []uuid.UUID{st.ID, st2.ID},
				offset:  cutil.GetPtr(5),
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all NVLinkInterfaces with search query as a status ready returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				searchQuery: cutil.GetPtr(NVLinkInterfaceStatusReady),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkInterfaces with search query as a status pending returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				searchQuery: cutil.GetPtr(NVLinkInterfaceStatusPending),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all ordered by created",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:     context.Background(),
				orderBy: &paginator.OrderBy{Field: "created", Order: paginator.OrderDescending},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantFirstEntry: &nvlinkInterfaces[29],
			wantErr:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvlisd := NVLinkInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}
			var instIds []uuid.UUID
			if tt.args.instanceIDs != nil {
				instIds = tt.args.instanceIDs
			}
			got, total, err := nvlisd.GetAll(
				tt.args.ctx,
				nil,
				NVLinkInterfaceFilterInput{
					InstanceIDs:               instIds,
					SiteIDs:                   tt.args.siteIDs,
					NVLinkLogicalPartitionIDs: tt.args.nvlinkLogicalPartitionIDs,
					Statuses:                  tt.args.statuses,
					NVLinkInterfaceIDs:        tt.args.ids,
					SearchQuery:               tt.args.searchQuery,
				},
				paginator.PageInput{
					Offset:  tt.args.offset,
					Limit:   tt.args.limit,
					OrderBy: tt.args.orderBy,
				},
				tt.args.paramRelations,
			)
			if tt.wantErr {
				require.Error(t, err)
			}

			assert.Equal(t, tt.wantCount, len(got))
			assert.Equal(t, tt.wantTotalCount, total)

			if len(got) > 0 && len(tt.args.paramRelations) > 0 {
				assert.NotNil(t, got[0].Site)
				assert.NotNil(t, got[0].Instance)
				assert.NotNil(t, got[0].NVLinkLogicalPartition)
			}

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.ID, got[0].ID)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx                      context.Context
		siteID                   uuid.UUID
		instanceID               uuid.UUID
		nvlinkLogicalPartitionID uuid.UUID
		device                   *string
		DeviceInstance           int
		status                   string
		createdBy                User
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	// Create necessary objects for instance
	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	i1, err := isd.Create(
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
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	nvli := &NVLinkInterface{
		SiteID:                   st.ID,
		InstanceID:               i1.ID,
		NVLinkLogicalPartitionID: nvllp.ID,
		Device:                   cutil.GetPtr("Nvidia GB200"),
		DeviceInstance:           0,
		Status:                   NVLinkInterfaceStatusPending,
		CreatedBy:                tnu.ID,
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *NVLinkInterface
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create NVLinkInterface from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                      ctx,
				siteID:                   nvli.SiteID,
				instanceID:               nvli.InstanceID,
				nvlinkLogicalPartitionID: nvli.NVLinkLogicalPartitionID,
				device:                   nvli.Device,
				DeviceInstance:           nvli.DeviceInstance,
				status:                   nvli.Status,
				createdBy:                User{ID: nvli.CreatedBy},
			},
			want:               nvli,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvlisd := NVLinkInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}
			got, err := nvlisd.Create(
				tt.args.ctx,
				nil,
				NVLinkInterfaceCreateInput{
					InstanceID:               tt.args.instanceID,
					SiteID:                   tt.args.siteID,
					NVLinkLogicalPartitionID: tt.args.nvlinkLogicalPartitionID,
					Device:                   tt.args.device,
					DeviceInstance:           tt.args.DeviceInstance,
					Status:                   tt.args.status,
					CreatedBy:                tt.args.createdBy.ID,
				},
			)
			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, tt.want.InstanceID, got.InstanceID)
			assert.Equal(t, tt.want.NVLinkLogicalPartitionID, got.NVLinkLogicalPartitionID)
			assert.Equal(t, tt.want.Device, got.Device)
			assert.Equal(t, tt.want.DeviceInstance, got.DeviceInstance)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	// Create necessary objects for instance
	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	i1, err := isd.Create(
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
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)
	nvli := testBuildNVLinkInterface(t, dbSession, nil, st.ID, i1.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 0, cutil.GetPtr("guid"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)

	uNVLinkInterface := nvli
	uNVLinkInterface.DeviceInstance = 1
	uNVLinkInterface.Status = NVLinkInterfaceStatusPending

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx             context.Context
		id              uuid.UUID
		DeviceInstance  int
		Status          string
		IsMissingOnSite bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *NVLinkInterface
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update NVLinkInterface from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            ctx,
				id:             nvli.ID,
				DeviceInstance: uNVLinkInterface.DeviceInstance,
				Status:         uNVLinkInterface.Status,
			},
			want:               uNVLinkInterface,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvlisd := NVLinkInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}
			got, err := nvlisd.Update(
				tt.args.ctx,
				nil,
				NVLinkInterfaceUpdateInput{
					NVLinkInterfaceID: tt.args.id,
					DeviceInstance:    &tt.args.DeviceInstance,
					Status:            &tt.args.Status,
				},
			)

			fmt.Printf("\ngot ID: %v, Created: %v, Updated: %v", got.ID.String(), got.Created, got.Updated)

			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, tt.want.DeviceInstance, got.DeviceInstance)
			assert.Equal(t, tt.want.Status, got.Status)

			assert.NotEqualValues(t, got.Updated, nvli.Updated)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_UpdateMultiple(t *testing.T) {
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
	instance, err := isd.Create(
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
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	// Create multiple NVLinkInterfaces for batch update testing
	nvli1 := testBuildNVLinkInterface(t, dbSession, nil, st.ID, instance.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 0, cutil.GetPtr("guid1"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)
	nvli2 := testBuildNVLinkInterface(t, dbSession, nil, st.ID, instance.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 1, cutil.GetPtr("guid2"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)
	nvli3 := testBuildNVLinkInterface(t, dbSession, nil, st.ID, instance.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 2, cutil.GetPtr("guid3"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)

	nvlisd := NewNVLinkInterfaceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []NVLinkInterfaceUpdateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "update batch of three nvlink interfaces",
			inputs: []NVLinkInterfaceUpdateInput{
				{
					NVLinkInterfaceID: nvli1.ID,
					Status:            cutil.GetPtr(NVLinkInterfaceStatusDeleting),
				},
				{
					NVLinkInterfaceID: nvli2.ID,
					Status:            cutil.GetPtr(NVLinkInterfaceStatusDeleting),
				},
				{
					NVLinkInterfaceID: nvli3.ID,
					Status:            cutil.GetPtr(NVLinkInterfaceStatusPending),
					DeviceInstance:    cutil.GetPtr(10),
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "update batch with empty input",
			inputs:             []NVLinkInterfaceUpdateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := nvlisd.UpdateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify results are returned in the same order as inputs
				for i, nvli := range got {
					assert.Equal(t, tc.inputs[i].NVLinkInterfaceID, nvli.ID, "result order should match input order")
					if tc.inputs[i].Status != nil {
						assert.Equal(t, *tc.inputs[i].Status, nvli.Status)
					}
					if tc.inputs[i].DeviceInstance != nil {
						assert.Equal(t, *tc.inputs[i].DeviceInstance, nvli.DeviceInstance)
					}
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_UpdateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	nvlisd := NewNVLinkInterfaceDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]NVLinkInterfaceUpdateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = NVLinkInterfaceUpdateInput{
			NVLinkInterfaceID: uuid.New(),
			Status:            cutil.GetPtr(NVLinkInterfaceStatusDeleting),
		}
	}

	_, err := nvlisd.UpdateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

func TestNVLinkInterfaceSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	TestSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	// Create necessary objects for instance
	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	i1, err := isd.Create(
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
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)
	nvli := testBuildNVLinkInterface(t, dbSession, nil, st.ID, i1.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 1, cutil.GetPtr("guid"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession  *db.Session
		tracerSpan *stracer.TracerSpan
	}
	type args struct {
		ctx    context.Context
		tx     *db.Tx
		id     uuid.UUID
		Device bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "clearing NVLinkInterface attributes returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    ctx,
				id:     nvli.ID,
				Device: true,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvlisd := NVLinkInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: tt.fields.tracerSpan,
			}
			got, err := nvlisd.Clear(
				tt.args.ctx,
				tt.args.tx,
				NVLinkInterfaceClearInput{
					NVLinkInterfaceID: tt.args.id,
					Device:            tt.args.Device,
				},
			)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}

			if tt.args.Device {
				assert.Nil(t, got.Device)
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
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

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	// Create necessary objects for instance
	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	i1, err := isd.Create(
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
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)
	nvli := testBuildNVLinkInterface(t, dbSession, nil, st.ID, i1.ID, nvllp.ID, nil, cutil.GetPtr("Nvidia GB200"), 0, cutil.GetPtr("guid"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "delete NVLinkInterface by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  nvli.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvlisd := NVLinkInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			err := nvlisd.Delete(tt.args.ctx, nil, tt.args.id)
			require.Equal(t, tt.wantErr, err != nil)

			dNVLinkInterface := &NVLinkInterface{}
			err = dbSession.DB.NewSelect().Model(dNVLinkInterface).WhereDeleted().Where("id = ?", nvli.ID).Scan(context.Background())
			require.NoError(t, err)
			assert.NotNil(t, dNVLinkInterface.Deleted)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_CreateMultiple(t *testing.T) {
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
	instance, err := isd.Create(
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

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	nvlisd := NewNVLinkInterfaceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []NVLinkInterfaceCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three nvlink interfaces",
			inputs: []NVLinkInterfaceCreateInput{
				{
					InstanceID:               instance.ID,
					SiteID:                   st.ID,
					NVLinkLogicalPartitionID: nvllp.ID,
					DeviceInstance:           0,
					Status:                   NVLinkInterfaceStatusPending,
					CreatedBy:                tnu.ID,
				},
				{
					InstanceID:               instance.ID,
					SiteID:                   st.ID,
					NVLinkLogicalPartitionID: nvllp.ID,
					DeviceInstance:           1,
					Status:                   NVLinkInterfaceStatusReady,
					CreatedBy:                tnu.ID,
				},
				{
					InstanceID:               instance.ID,
					SiteID:                   st.ID,
					NVLinkLogicalPartitionID: nvllp.ID,
					DeviceInstance:           2,
					Status:                   NVLinkInterfaceStatusPending,
					CreatedBy:                tnu.ID,
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []NVLinkInterfaceCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := nvlisd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify results are returned in the same order as inputs
				for i, nvli := range got {
					assert.NotEqual(t, uuid.Nil, nvli.ID)
					assert.Equal(t, tc.inputs[i].DeviceInstance, nvli.DeviceInstance, "result order should match input order")
					assert.Equal(t, tc.inputs[i].Status, nvli.Status)
					assert.NotZero(t, nvli.Created)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkInterfaceSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	nvlisd := NewNVLinkInterfaceDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]NVLinkInterfaceCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = NVLinkInterfaceCreateInput{
			InstanceID:     uuid.New(),
			DeviceInstance: 0,
			Status:         NVLinkInterfaceStatusPending,
		}
	}

	_, err := nvlisd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

func TestNVLinkInterfaceSQLDAO_DeleteAllBySiteID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	TestSetupSchema(t, dbSession)

	// Shared infrastructure
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	// Two target sites plus a third site that has no NVLink interfaces, used to
	// confirm DeleteAllBySiteID is a no-op when nothing matches.
	st1 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-1", "Test Site 1", ip.Org, ipu.ID)
	st2 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-2", "Test Site 2", ip.Org, ipu.ID)
	st3 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-3", "Test Site 3", ip.Org, ipu.ID)

	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")

	buildInstanceForSite := func(site *Site, hostname, machineTag string) *Instance {
		vpc := testInstanceBuildVpc(t, dbSession, ip, site, tn, "vpc-"+site.Name)
		machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr(machineTag))
		alloc := testInstanceBuildAllocation(t, dbSession, ip, tn, site, "alloc-"+site.Name)
		_ = testBuildAllocationConstraint(t, dbSession, alloc, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())

		isd := NewInstanceDAO(dbSession)
		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     "instance-" + site.Name,
				TenantID:                 tn.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				InstanceTypeID:           &instanceType.ID,
				VpcID:                    vpc.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr(hostname),
				OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				UserData:                 cutil.GetPtr("userdata"),
				InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
				Status:                   InstanceStatusPending,
				CreatedBy:                tnu.ID,
			},
		)
		require.NoError(t, err)
		return instance
	}

	inst1 := buildInstanceForSite(st1, "host1.com", "mcType1")
	inst2 := buildInstanceForSite(st2, "host2.com", "mcType2")

	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, nil, "nvllp-site-1", nil, tn.Org, tn.ID, st1.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)
	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, nil, "nvllp-site-2", nil, tn.Org, tn.ID, st2.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	// Two interfaces in the target site, one in another site that should remain.
	nvli1a := testBuildNVLinkInterface(t, dbSession, nil, st1.ID, inst1.ID, nvllp1.ID, nil, cutil.GetPtr("Nvidia GB200"), 0, cutil.GetPtr("guid-1a"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)
	nvli1b := testBuildNVLinkInterface(t, dbSession, nil, st1.ID, inst1.ID, nvllp1.ID, nil, cutil.GetPtr("Nvidia GB200"), 1, cutil.GetPtr("guid-1b"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)
	nvli2 := testBuildNVLinkInterface(t, dbSession, nil, st2.ID, inst2.ID, nvllp2.ID, nil, cutil.GetPtr("Nvidia GB200"), 0, cutil.GetPtr("guid-2"), cutil.GetPtr(NVLinkInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	nvlisd := NewNVLinkInterfaceDAO(dbSession)

	// Delete all NVLink interfaces under st1.
	err := nvlisd.DeleteAllBySiteID(ctx, nil, st1.ID)
	require.NoError(t, err)

	// Both st1 interfaces should be soft-deleted.
	for _, id := range []uuid.UUID{nvli1a.ID, nvli1b.ID} {
		deleted := &NVLinkInterface{}
		err = dbSession.DB.NewSelect().Model(deleted).WhereDeleted().Where("id = ?", id).Scan(context.Background())
		require.NoError(t, err, "expected soft-deleted row for id %s", id)
		assert.NotNil(t, deleted.Deleted)

		// Default selects (which exclude soft-deleted rows) should not return them.
		notFound := &NVLinkInterface{}
		err = dbSession.DB.NewSelect().Model(notFound).Where("id = ?", id).Scan(context.Background())
		require.Error(t, err, "soft-deleted row for id %s should not appear in default selects", id)
	}

	// The interface scoped to the other site must be left untouched.
	other := &NVLinkInterface{}
	err = dbSession.DB.NewSelect().Model(other).Where("id = ?", nvli2.ID).Scan(context.Background())
	require.NoError(t, err)
	assert.Nil(t, other.Deleted)

	// Calling DeleteAllBySiteID for a site with no interfaces should be a no-op.
	err = nvlisd.DeleteAllBySiteID(ctx, nil, st3.ID)
	require.NoError(t, err)

	// Verify the active span is propagated through the call.
	span := otrace.SpanFromContext(ctx)
	assert.True(t, span.SpanContext().IsValid())
}
