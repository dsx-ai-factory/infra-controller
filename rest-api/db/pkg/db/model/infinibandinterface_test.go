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

func testInfiniBandInterfaceSetupSchema(t *testing.T, dbSession *db.Session) {
	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*InfiniBandPartition)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*InfiniBandInterface)(nil))
	require.NoError(t, err)
}

func TestInfiniBandInterfaceSQLDAO_GetByID(t *testing.T) {
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
	testInfiniBandInterfaceSetupSchema(t, dbSession)

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

	ibpr := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)
	ibif := testBuildInfiniBandInterface(t, dbSession, nil, st.ID, i1.ID, ibpr.ID, 1, false, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *InfiniBandInterface
		wantErr            error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			name: "get InfiniBandInterface by ID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  ibpr.ID,
			},
			want:               ibif,
			wantErr:            nil,
			paramRelations:     []string{SiteRelationName, InfiniBandPartitionRelationName},
			verifyChildSpanner: true,
		},
		{
			name: "get InfiniBandInterface by non-existent ID returns error",
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
			ibifsd := InfiniBandInterfaceSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			got, err := ibifsd.GetByID(tt.args.ctx, nil, tt.args.id, tt.paramRelations)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if err == nil {
				if len(tt.paramRelations) > 0 {
					assert.NotNil(t, got.Site)
					assert.NotNil(t, got.InfiniBandPartition)
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

func TestInfiniBandInterface_GetAll(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx                     context.Context
		ids                     []uuid.UUID
		siteIDs                 []uuid.UUID
		instanceIDs             []uuid.UUID
		inifiniBandPartitionIDs []uuid.UUID
		searchQuery             *string
		statuses                []string
		offset                  *int
		limit                   *int
		orderBy                 *paginator.OrderBy
		paramRelations          []string
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testInfiniBandInterfaceSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))

	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu1 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("janed@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn1 := testBuildTenant(t, dbSession, nil, "test-tenant-1", "test-tenant-org-1", tnu1.ID)

	tnu2 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jimd@test.com"), cutil.GetPtr("Jim"), cutil.GetPtr("Doe"))
	tn2 := testBuildTenant(t, dbSession, nil, "test-tenant-2", "test-tenant-org-2", tnu2.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)
	st2 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-2", "Test Site", ip.Org, ipu.ID)

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

	ibpr := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition", nil, tn1.Org, tn1.ID, st.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu1.ID)
	ibpr2 := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition2", nil, tn2.Org, tn2.ID, st2.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu2.ID)

	totalCount := 30
	infiniBandInterfaces := []InfiniBandInterface{}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	for i := 0; i < totalCount; i++ {
		var ibif *InfiniBandInterface
		var tn *Tenant

		if i%2 == 0 {
			tn = tn1
		} else {
			tn = tn2
		}

		if i%2 == 0 {
			ibif = testBuildInfiniBandInterface(t, dbSession, nil, st.ID, i1.ID, ibpr.ID, 1, false, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tn.ID)
		} else {
			ibif = testBuildInfiniBandInterface(t, dbSession, nil, st2.ID, i2.ID, ibpr2.ID, 1, false, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusPending), tn.ID)
		}

		infiniBandInterfaces = append(infiniBandInterfaces, *ibif)
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		wantFirstEntry     *InfiniBandInterface
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get all InfiniBandInterfaces with no filters returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                     ctx,
				siteIDs:                 nil,
				instanceIDs:             nil,
				inifiniBandPartitionIDs: nil,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all InfiniBandInterfaces with relation returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                     context.Background(),
				siteIDs:                 nil,
				instanceIDs:             nil,
				inifiniBandPartitionIDs: nil,
				paramRelations:          []string{InstanceRelationName, SiteRelationName, InfiniBandPartitionRelationName},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all InfiniBandInterfaces with Site ID filter returns success",
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
			name: "get all InfiniBandInterfaces with Site ID filter with multiple values returns success",
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
			name: "get all InfiniBandInterfaces with Instance ID and name filters returns success",
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
			name: "get all InfiniBandInterfaces with Partition ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                     context.Background(),
				inifiniBandPartitionIDs: []uuid.UUID{ibpr.ID},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
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
			name: "get all InfiniBandInterfaces with search query as a status ready returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				searchQuery: cutil.GetPtr(InfiniBandInterfaceStatusReady),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all InfiniBandInterfaces with search query as a status pending returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				searchQuery: cutil.GetPtr(InfiniBandInterfaceStatusPending),
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
			wantFirstEntry: &infiniBandInterfaces[29],
			wantErr:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ibifsd := InfiniBandInterfaceSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			var instIds []uuid.UUID
			if tt.args.instanceIDs != nil {
				instIds = tt.args.instanceIDs
			}
			got, total, err := ibifsd.GetAll(
				tt.args.ctx,
				nil,
				InfiniBandInterfaceFilterInput{
					InstanceIDs:            instIds,
					SiteIDs:                tt.args.siteIDs,
					InfiniBandPartitionIDs: tt.args.inifiniBandPartitionIDs,
					Statuses:               tt.args.statuses,
					InfiniBandInterfaceIDs: tt.args.ids,
					SearchQuery:            tt.args.searchQuery,
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
				assert.NotNil(t, got[0].InfiniBandPartition)
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

func TestInfiniBandInterfaceSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx                    context.Context
		siteID                 uuid.UUID
		instanceID             uuid.UUID
		inifiniBandPartitionID uuid.UUID
		device                 string
		vendor                 *string
		deviceInstance         int
		isPhysical             bool
		virtualFunctionID      *int
		physical_guid          *string
		guid                   *string
		status                 string
		createdBy              User
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testInfiniBandPartitionSetupSchema(t, dbSession)

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

	ibpr := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)

	ibif := &InfiniBandInterface{
		SiteID:                st.ID,
		InstanceID:            i1.ID,
		InfiniBandPartitionID: ibpr.ID,
		Device:                "mks-123",
		Vendor:                cutil.GetPtr("Mellenox"),
		DeviceInstance:        1,
		IsPhysical:            true,
		VirtualFunctionID:     cutil.GetPtr(1),
		GUID:                  cutil.GetPtr("guid"),
		Status:                InfiniBandInterfaceStatusPending,
		CreatedBy:             tnu.ID,
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *InfiniBandInterface
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create InfiniBandInterface from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                    ctx,
				siteID:                 ibif.SiteID,
				instanceID:             ibif.InstanceID,
				inifiniBandPartitionID: ibif.InfiniBandPartitionID,
				device:                 ibif.Device,
				vendor:                 ibif.Vendor,
				deviceInstance:         ibif.DeviceInstance,
				isPhysical:             ibif.IsPhysical,
				virtualFunctionID:      ibif.VirtualFunctionID,
				physical_guid:          ibif.PhysicalGUID,
				guid:                   ibif.GUID,
				status:                 ibif.Status,
				createdBy:              User{ID: ibif.CreatedBy},
			},
			want:               ibif,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ibifsd := InfiniBandInterfaceSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ibifsd.Create(
				tt.args.ctx,
				nil,
				InfiniBandInterfaceCreateInput{
					InstanceID:            tt.args.instanceID,
					SiteID:                tt.args.siteID,
					InfiniBandPartitionID: tt.args.inifiniBandPartitionID,
					Device:                tt.args.device,
					Vendor:                tt.args.vendor,
					DeviceInstance:        tt.args.deviceInstance,
					IsPhysical:            tt.args.isPhysical,
					VirtualFunctionID:     tt.args.virtualFunctionID,
					PhysicalGUID:          tt.args.physical_guid,
					GUID:                  tt.args.guid,
					Status:                tt.args.status,
					CreatedBy:             tt.args.createdBy.ID,
				},
			)
			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, tt.want.InstanceID, got.InstanceID)
			assert.Equal(t, tt.want.InfiniBandPartitionID, got.InfiniBandPartitionID)
			assert.Equal(t, tt.want.DeviceInstance, got.DeviceInstance)
			assert.Equal(t, tt.want.IsPhysical, got.IsPhysical)
			assert.Equal(t, tt.want.VirtualFunctionID, got.VirtualFunctionID)
			assert.Equal(t, tt.want.IsMissingOnSite, got.IsMissingOnSite)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandInterfaceSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testInfiniBandInterfaceSetupSchema(t, dbSession)

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

	ibpr := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)
	ibif := testBuildInfiniBandInterface(t, dbSession, nil, st.ID, i1.ID, ibpr.ID, 1, false, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)

	uInfiniBandInterface := ibif
	uInfiniBandInterface.VirtualFunctionID = cutil.GetPtr(2)
	uInfiniBandInterface.Status = InfiniBandInterfaceStatusPending
	uInfiniBandInterface.IsMissingOnSite = true

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx               context.Context
		id                uuid.UUID
		virtualFunctionID *int
		Status            string
		IsMissingOnSite   bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *InfiniBandInterface
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update InfiniBandInterface from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:               ctx,
				id:                ibif.ID,
				virtualFunctionID: uInfiniBandInterface.VirtualFunctionID,
				Status:            uInfiniBandInterface.Status,
				IsMissingOnSite:   uInfiniBandInterface.IsMissingOnSite,
			},
			want:               uInfiniBandInterface,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ibifsd := InfiniBandInterfaceSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ibifsd.Update(
				tt.args.ctx,
				nil,
				InfiniBandInterfaceUpdateInput{
					InfiniBandInterfaceID: tt.args.id,
					VirtualFunctionId:     tt.args.virtualFunctionID,
					Status:                &tt.args.Status,
					IsMissingOnSite:       &tt.args.IsMissingOnSite,
				},
			)

			fmt.Printf("\ngot ID: %v, Created: %v, Updated: %v", got.ID.String(), got.Created, got.Updated)

			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, *tt.want.VirtualFunctionID, *got.VirtualFunctionID)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.IsMissingOnSite, got.IsMissingOnSite)

			assert.NotEqualValues(t, got.Updated, ibif.Updated)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandInterfaceSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testInfiniBandInterfaceSetupSchema(t, dbSession)

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

	ibpr := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)
	ibif := testBuildInfiniBandInterface(t, dbSession, nil, st.ID, i1.ID, ibpr.ID, 1, false, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession  *db.Session
		tracerSpan *stracer.TracerSpan
	}
	type args struct {
		ctx               context.Context
		tx                *db.Tx
		id                uuid.UUID
		virtualFunctionID bool
		guid              bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "clearing InfiniBandInterface attributes returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:               ctx,
				id:                ibif.ID,
				virtualFunctionID: true,
				guid:              true,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ibifsd := InfiniBandInterfaceSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: tt.fields.tracerSpan,
			}
			got, err := ibifsd.Clear(
				tt.args.ctx,
				tt.args.tx,
				InfiniBandInterfaceClearInput{
					InfiniBandInterfaceID: tt.args.id,
					Vendor:                false,
					VirtualFunctionId:     tt.args.virtualFunctionID,
					PhysicalGUID:          false,
					GUID:                  tt.args.guid,
				},
			)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}

			if tt.args.virtualFunctionID {
				assert.Nil(t, got.VirtualFunctionID)
			}

			if tt.args.guid {
				assert.Nil(t, got.GUID)
			}
		})
	}
}

func TestInfiniBandInterfaceSQLDAO_Delete(t *testing.T) {
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
	testInfiniBandInterfaceSetupSchema(t, dbSession)

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

	ibpr := testBuildInfiniBandPartition(t, dbSession, nil, "test-infinibandpartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)
	ibif := testBuildInfiniBandInterface(t, dbSession, nil, st.ID, i1.ID, ibpr.ID, 1, false, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)

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
			name: "delete InfiniBandInterface by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  ibif.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ibifsd := InfiniBandInterfaceSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			err := ibifsd.Delete(tt.args.ctx, nil, tt.args.id)
			require.Equal(t, tt.wantErr, err != nil)

			dInfiniBandInterface := &InfiniBandInterface{}
			err = dbSession.DB.NewSelect().Model(dInfiniBandInterface).WhereDeleted().Where("id = ?", ibif.ID).Scan(context.Background())
			require.NoError(t, err)
			assert.NotNil(t, dInfiniBandInterface.Deleted)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandInterfaceSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testInfiniBandInterfaceSetupSchema(t, dbSession)

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

	ibp := testBuildInfiniBandPartition(t, dbSession, nil, "test-ibpartition", cutil.GetPtr("Test IB Partition"), tn.Org, tn.ID, st.ID, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)

	ibisd := NewInfiniBandInterfaceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []InfiniBandInterfaceCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three infiniband interfaces",
			inputs: []InfiniBandInterfaceCreateInput{
				{
					InstanceID:            instance.ID,
					SiteID:                st.ID,
					InfiniBandPartitionID: ibp.ID,
					Device:                "mlx5_0",
					DeviceInstance:        0,
					IsPhysical:            true,
					Status:                InfiniBandInterfaceStatusPending,
					CreatedBy:             tnu.ID,
				},
				{
					InstanceID:            instance.ID,
					SiteID:                st.ID,
					InfiniBandPartitionID: ibp.ID,
					Device:                "mlx5_1",
					DeviceInstance:        1,
					IsPhysical:            true,
					Status:                InfiniBandInterfaceStatusReady,
					CreatedBy:             tnu.ID,
				},
				{
					InstanceID:            instance.ID,
					SiteID:                st.ID,
					InfiniBandPartitionID: ibp.ID,
					Device:                "mlx5_2",
					DeviceInstance:        2,
					IsPhysical:            false,
					Status:                InfiniBandInterfaceStatusPending,
					CreatedBy:             tnu.ID,
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []InfiniBandInterfaceCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ibisd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify results are returned in the same order as inputs
				for i, ibi := range got {
					assert.NotEqual(t, uuid.Nil, ibi.ID)
					assert.Equal(t, tc.inputs[i].Device, ibi.Device, "result order should match input order")
					assert.Equal(t, tc.inputs[i].Status, ibi.Status)
					assert.NotZero(t, ibi.Created)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandInterfaceSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	ibisd := NewInfiniBandInterfaceDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]InfiniBandInterfaceCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = InfiniBandInterfaceCreateInput{
			InstanceID: uuid.New(),
			Device:     "device-test",
			Status:     InfiniBandInterfaceStatusPending,
		}
	}

	_, err := ibisd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

func TestInfiniBandInterfaceSQLDAO_DeleteAllBySiteID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testInfiniBandInterfaceSetupSchema(t, dbSession)

	// Shared infrastructure
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	// Two target sites plus a third site that has no IB interfaces, used to
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

	ibp1 := testBuildInfiniBandPartition(t, dbSession, nil, "ibp-site-1", nil, tn.Org, tn.ID, st1.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)
	ibp2 := testBuildInfiniBandPartition(t, dbSession, nil, "ibp-site-2", nil, tn.Org, tn.ID, st2.ID, cutil.GetPtr(uuid.New()), nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(InfiniBandPartitionStatusReady), tnu.ID)

	// Two interfaces in the target site, one in another site that should remain.
	ibi1a := testBuildInfiniBandInterface(t, dbSession, nil, st1.ID, inst1.ID, ibp1.ID, 1, true, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)
	ibi1b := testBuildInfiniBandInterface(t, dbSession, nil, st1.ID, inst1.ID, ibp1.ID, 2, true, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)
	ibi2 := testBuildInfiniBandInterface(t, dbSession, nil, st2.ID, inst2.ID, ibp2.ID, 1, true, nil, nil, false, cutil.GetPtr(InfiniBandInterfaceStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	ibisd := NewInfiniBandInterfaceDAO(dbSession)

	// Delete all IB interfaces under st1.
	err := ibisd.DeleteAllBySiteID(ctx, nil, st1.ID)
	require.NoError(t, err)

	// Both st1 interfaces should be soft-deleted.
	for _, id := range []uuid.UUID{ibi1a.ID, ibi1b.ID} {
		deleted := &InfiniBandInterface{}
		err = dbSession.DB.NewSelect().Model(deleted).WhereDeleted().Where("id = ?", id).Scan(context.Background())
		require.NoError(t, err, "expected soft-deleted row for id %s", id)
		assert.NotNil(t, deleted.Deleted)

		// Default selects (which exclude soft-deleted rows) should not return them.
		notFound := &InfiniBandInterface{}
		err = dbSession.DB.NewSelect().Model(notFound).Where("id = ?", id).Scan(context.Background())
		assert.Error(t, err, "soft-deleted row for id %s should not appear in default selects", id)
	}

	// The interface scoped to the other site must be left untouched.
	other := &InfiniBandInterface{}
	err = dbSession.DB.NewSelect().Model(other).Where("id = ?", ibi2.ID).Scan(context.Background())
	require.NoError(t, err)
	assert.Nil(t, other.Deleted)

	// Calling DeleteAllBySiteID for a site with no interfaces should be a no-op.
	err = ibisd.DeleteAllBySiteID(ctx, nil, st3.ID)
	require.NoError(t, err)

	// Verify the active span is propagated through the call.
	span := otrace.SpanFromContext(ctx)
	assert.True(t, span.SpanContext().IsValid())
}
