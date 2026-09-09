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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/proto"
)

func testVpcSetupSchema(t *testing.T, dbSession *db.Session) {
	// Create tables

	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*NVLinkLogicalPartition)(nil))
	require.NoError(t, err)

	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	require.NoError(t, err)
}

func TestVpcSQLDAO_GetByID(t *testing.T) {
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
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn, st, "testNetworkSecurityGroup")

	vpc := testBuildVpc(t, dbSession, nil, "test-vpc", nil, tn.Org, ip.ID, tn.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(VpcStatusReady), tnu.ID, &networkSecurityGroup.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *Vpc
		wantErr            error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			name: "get VPC by ID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  vpc.ID,
			},
			want:               vpc,
			wantErr:            nil,
			paramRelations:     []string{TenantRelationName, SiteRelationName, NetworkSecurityGroupRelationName},
			verifyChildSpanner: true,
		},
		{
			name: "get VPC by non-existent ID returns error",
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
			vsd := VpcSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			got, err := vsd.GetByID(tt.args.ctx, nil, tt.args.id, tt.paramRelations)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if err == nil {
				if len(tt.paramRelations) > 0 {
					assert.NotNil(t, got.Site)
					assert.NotNil(t, got.Tenant)
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

func TestVpcSQLDAO_GetCountByStatus(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu1 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe1@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tnu2 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe2@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))

	tn1 := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu1.ID)
	assert.NotNil(t, tn1)

	tn2 := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu2.ID)
	assert.NotNil(t, tn2)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)
	assert.NotNil(t, st)

	vpc := testBuildVpc(t, dbSession, nil, "test-vpc", nil, tn1.Org, ip.ID, tn1.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(VpcStatusReady), tnu1.ID, nil)
	assert.NotNil(t, vpc)
	vpc2 := testBuildVpc(t, dbSession, nil, "test-vpc-1", nil, tn1.Org, ip.ID, tn1.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(VpcStatusDeleting), tnu1.ID, nil)
	assert.NotNil(t, vpc2)
	vpc3 := testBuildVpc(t, dbSession, nil, "test-vpc-1", nil, tn1.Org, ip.ID, tn1.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(VpcStatusReady), tnu1.ID, nil)
	assert.NotNil(t, vpc3)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name                        string
		fields                      fields
		args                        args
		wantErr                     error
		wantEmpty                   bool
		wantCount                   int
		wantStatusMap               map[string]int
		reqInfrastructureProviderID *uuid.UUID
		reqTenant                   *uuid.UUID
		reqSite                     *uuid.UUID
		reqOrg                      *string
		paramRelations              []string
		verifyChildSpanner          bool
	}{
		{
			name: "get vpc status count by tenant with vpc returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 3,
			wantStatusMap: map[string]int{
				VpcStatusError:        0,
				VpcStatusProvisioning: 0,
				VpcStatusPending:      0,
				VpcStatusDeleting:     1,
				VpcStatusReady:        2,
				"total":               3,
			},
			reqTenant:          cutil.GetPtr(tn1.ID),
			verifyChildSpanner: true,
		},
		{
			name: "get vpc status count by tenant with no vpc returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: true,
			reqTenant: cutil.GetPtr(tn2.ID),
		},
		{
			name: "get vpc status count with no filter vpc returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantCount: 3,
			wantStatusMap: map[string]int{
				VpcStatusError:        0,
				VpcStatusProvisioning: 0,
				VpcStatusPending:      0,
				VpcStatusDeleting:     1,
				VpcStatusReady:        2,
				"total":               3,
			},
			wantErr:   nil,
			wantEmpty: false,
		},
		{
			name: "get vpc status count by infrastructure provider with vpc returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 3,
			wantStatusMap: map[string]int{
				VpcStatusError:        0,
				VpcStatusProvisioning: 0,
				VpcStatusPending:      0,
				VpcStatusDeleting:     1,
				VpcStatusReady:        2,
				"total":               3,
			},
			reqInfrastructureProviderID: cutil.GetPtr(ip.ID),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vsd := VpcSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := vsd.GetCountByStatus(tt.args.ctx, nil, tt.reqInfrastructureProviderID, tt.reqTenant, tt.reqSite)
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
					assert.EqualValues(t, got[VpcStatusDeleting], 1)
					assert.EqualValues(t, got[VpcStatusReady], 2)
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

func TestVpc_GetAll(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx                       context.Context
		name                      *string
		infrastructureProviderID  *uuid.UUID
		tenantID                  *uuid.UUID
		siteID                    *uuid.UUID
		NVLinkLogicalPartitionID  *uuid.UUID
		VpcIDs                    []uuid.UUID
		org                       *string
		networkVirtualizationType *string
		searchQuery               *string
		status                    *string
		offset                    *int
		limit                     *int
		orderBy                   *paginator.OrderBy
		paramRelations            []string
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))

	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu1 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("janed@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn1 := testBuildTenant(t, dbSession, nil, "test-tenant-1", "test-tenant-org-1", tnu1.ID)

	tnu2 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jimd@test.com"), cutil.GetPtr("Jim"), cutil.GetPtr("Doe"))
	tn2 := testBuildTenant(t, dbSession, nil, "test-tenant-2", "test-tenant-org-2", tnu2.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn1, st, "testNetworkSecurityGroup")

	nvlinkLogicalPartition := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-nvlinklogicalpartition", nil, tn1.Org, tn1.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu1.ID)

	totalCount := 30

	vpcs := []Vpc{}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	for i := 0; i < totalCount; i++ {
		var vpc *Vpc
		var tn *Tenant

		if i%2 == 0 {
			tn = tn1
		} else {
			tn = tn2
		}

		if i%2 == 0 {
			vpc = testBuildVpc(t, dbSession, nil, fmt.Sprintf("test-vpc-batch-v1-%v", i), cutil.GetPtr(fmt.Sprintf("test-vpc-desc-batch-1-%v", i)), tn.Org, ip.ID, tn.ID, st.ID, cutil.GetPtr(nvlinkLogicalPartition.ID), cutil.GetPtr(VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), map[string]string{fmt.Sprintf("test-vpc-batch-key1-%v", i): fmt.Sprintf("test-vpc-batch-value1-%v", i)}, cutil.GetPtr(VpcStatusReady), tn.CreatedBy, &networkSecurityGroup.ID)
		} else {
			vpc = testBuildVpc(t, dbSession, nil, fmt.Sprintf("test-vpc-batch-v2-%v", i), cutil.GetPtr(fmt.Sprintf("test-vpc-desc-batch-2-%v", i)), tn.Org, ip.ID, tn.ID, st.ID, nil, cutil.GetPtr(VpcFNN), cutil.GetPtr(uuid.New()), map[string]string{fmt.Sprintf("test-vpc-batch-key2-%v", i): fmt.Sprintf("test-vpc-batch-value2-%v", i)}, cutil.GetPtr(VpcStatusDeleting), tn.CreatedBy, &networkSecurityGroup.ID)
		}

		vpcs = append(vpcs, *vpc)
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		wantFirstEntry     *Vpc
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get all Vpcs with filter on ID - success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    ctx,
				VpcIDs: []uuid.UUID{vpcs[1].ID, vpcs[2].ID},
			},
			wantCount:          2,
			wantTotalCount:     2,
			wantErr:            false,
			verifyChildSpanner: true,
		},

		{
			name: "get all Vpcs with no relation filter success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      ctx,
				tenantID: nil,
				siteID:   nil,
				org:      nil,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all Vpcs with relation returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            context.Background(),
				tenantID:       nil,
				siteID:         nil,
				org:            nil,
				paramRelations: []string{TenantRelationName, SiteRelationName},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with Tenant ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: &tn1.ID,
				siteID:   nil,
				org:      nil,
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with Tenant ID and name filters returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				name:     cutil.GetPtr("test-vpc-batch-v1-8"),
				tenantID: &tn1.ID,
				siteID:   nil,
				org:      nil,
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with Site ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: nil,
				siteID:   &st.ID,
				org:      nil,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with NVLink Logical Partition ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                      context.Background(),
				NVLinkLogicalPartitionID: &nvlinkLogicalPartition.ID,
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with Org filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: nil,
				siteID:   nil,
				org:      &tn1.Org,
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with tenant and Org filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: &tn1.ID,
				siteID:   nil,
				org:      &tn1.Org,
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with network virtulization type filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                       context.Background(),
				tenantID:                  nil,
				siteID:                    nil,
				org:                       nil,
				networkVirtualizationType: cutil.GetPtr(VpcFNN),
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
				ctx:      context.Background(),
				tenantID: nil,
				siteID:   &st.ID,
				org:      nil,
				limit:    cutil.GetPtr(10),
			},
			wantCount:      10,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all with offset returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: &tn1.ID,
				siteID:   nil,
				org:      nil,
				offset:   cutil.GetPtr(5),
			},
			wantCount:      10,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Sites ordered by name",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: &tn1.ID,
				siteID:   nil,
				org:      nil,
				orderBy:  &paginator.OrderBy{Field: "name", Order: paginator.OrderDescending},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantFirstEntry: &vpcs[8],
			wantErr:        false,
		},
		{
			name: "get all Vpcs with Org filter with site/tenant include relation returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            context.Background(),
				tenantID:       nil,
				siteID:         nil,
				org:            &tn1.Org,
				paramRelations: []string{SiteRelationName, TenantRelationName},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with infrastructure ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                      context.Background(),
				tenantID:                 nil,
				siteID:                   nil,
				infrastructureProviderID: &ip.ID,
				org:                      nil,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query as name returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-v1-"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query as a description returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-desc-batch-1-"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query as label key returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-key1-"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query as label value returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-value1-"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with exact key label string return success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-key1-6"),
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with exact value label string return success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-value2-7"),
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with exact key value label string return success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-value2-7 test-vpc-batch-key1-6"),
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with no label exits return none success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-desc-batch-key12-"),
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query as a status ready returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr(VpcStatusReady),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query as a status deleting returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr(VpcStatusDeleting),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with combination of name and status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-batch-v1- | ready"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with combination of description and status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-desc-batch-1- error"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with network virtulization type returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr(VpcFNN),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with search query with combination of description and status returns none success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr("test-vpc-desc-batch-3- error"),
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with empty search query returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantID:    nil,
				siteID:      nil,
				org:         nil,
				searchQuery: cutil.GetPtr(""),
			},
			wantCount:      20,
			wantTotalCount: 30,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with empty search query returns success with ip",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                      context.Background(),
				tenantID:                 nil,
				siteID:                   nil,
				infrastructureProviderID: &ip.ID,
				org:                      nil,
				searchQuery:              cutil.GetPtr(""),
			},
			wantCount:      20,
			wantTotalCount: 30,
			wantErr:        false,
		},
		{
			name: "get all Vpcs with status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:      context.Background(),
				tenantID: nil,
				siteID:   nil,
				org:      nil,
				status:   cutil.GetPtr(VpcStatusDeleting),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vsd := VpcSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			filterInput := VpcFilterInput{
				Name:                      tt.args.name,
				InfrastructureProviderID:  tt.args.infrastructureProviderID,
				Org:                       tt.args.org,
				NetworkVirtualizationType: tt.args.networkVirtualizationType,
				SearchQuery:               tt.args.searchQuery,
				VpcIDs:                    tt.args.VpcIDs,
			}

			if tt.args.tenantID != nil {
				filterInput.TenantIDs = []uuid.UUID{*tt.args.tenantID}
			}

			if tt.args.siteID != nil {
				filterInput.SiteIDs = []uuid.UUID{*tt.args.siteID}
			}
			if tt.args.NVLinkLogicalPartitionID != nil {
				filterInput.NVLinkLogicalPartitionIDs = []uuid.UUID{*tt.args.NVLinkLogicalPartitionID}
			}

			if tt.args.status != nil {
				filterInput.Statuses = []string{*tt.args.status}
			}

			pageInput := paginator.PageInput{
				Offset:  tt.args.offset,
				Limit:   tt.args.limit,
				OrderBy: tt.args.orderBy,
			}

			got, total, err := vsd.GetAll(tt.args.ctx, nil, filterInput, pageInput, tt.args.paramRelations)
			if tt.wantErr {
				require.Error(t, err)
			}

			assert.Equal(t, tt.wantCount, len(got))
			assert.Equal(t, tt.wantTotalCount, total)

			if len(got) > 0 && len(tt.args.paramRelations) > 0 {
				assert.NotNil(t, got[0].Site)
				assert.NotNil(t, got[0].Tenant)
			}

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.Name, got[0].Name)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestVpcSQLDAO_CreateFromParams(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx                                    context.Context
		name                                   string
		description                            *string
		org                                    string
		infrastructureProviderID               uuid.UUID
		tenantID                               uuid.UUID
		siteID                                 uuid.UUID
		networkVirtualizationType              *string
		slaacEnabled                           bool
		routingProfile                         *string
		routingProfileOverrides                *VpcRoutingProfileOverrides
		controllerVpcID                        *uuid.UUID
		activeVni                              *int
		networkSecurityGroupID                 *string
		networkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails
		labels                                 map[string]string
		status                                 string
		createdBy                              User
		vni                                    *int
		id                                     *uuid.UUID
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn, st, "testNetworkSecurityGroup")
	routingProfileOverrides := &VpcRoutingProfileOverrides{
		RouteTargetImports:           &[]VpcRouteTarget{{ASN: 64512, VNI: 101}},
		LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
		AllowedAnycastPrefixes:       &[]string{},
	}

	vpc := &Vpc{
		Name:                      "test-vpc",
		Description:               cutil.GetPtr("Test VPC"),
		Org:                       tn.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: cutil.GetPtr(VpcEthernetVirtualizer),
		SlaacEnabled:              true,
		RoutingProfile:            cutil.GetPtr("INTERNAL"),
		RoutingProfileOverrides:   routingProfileOverrides,
		ControllerVpcID:           cutil.GetPtr(uuid.New()),
		ActiveVni:                 nil,
		Vni:                       cutil.GetPtr(555),
		NetworkSecurityGroupID:    &networkSecurityGroup.ID,
		NetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{
			NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
				Id:                      "",
				RelatedInstanceIds:      []string{},
				UnpropagatedInstanceIds: []string{},
				Status:                  corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL,
			},
		},
		Labels: map[string]string{
			"zone1": "gpu",
			"zone2": "dpu",
		},
		Status:    VpcStatusPending,
		CreatedBy: tnu.ID,
		ID:        uuid.New(),
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *Vpc
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create VPC from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                                    ctx,
				name:                                   vpc.Name,
				description:                            vpc.Description,
				org:                                    vpc.Org,
				infrastructureProviderID:               vpc.InfrastructureProviderID,
				tenantID:                               vpc.TenantID,
				siteID:                                 vpc.SiteID,
				networkVirtualizationType:              vpc.NetworkVirtualizationType,
				slaacEnabled:                           vpc.SlaacEnabled,
				routingProfile:                         vpc.RoutingProfile,
				routingProfileOverrides:                vpc.RoutingProfileOverrides,
				controllerVpcID:                        vpc.ControllerVpcID,
				activeVni:                              vpc.ActiveVni,
				vni:                                    vpc.Vni,
				networkSecurityGroupID:                 vpc.NetworkSecurityGroupID,
				networkSecurityGroupPropagationDetails: vpc.NetworkSecurityGroupPropagationDetails,
				labels:                                 vpc.Labels,
				status:                                 vpc.Status,
				createdBy:                              User{ID: vpc.CreatedBy},
				id:                                     cutil.GetPtr(vpc.ID),
			},
			want:               vpc,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vsd := VpcSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			input := VpcCreateInput{
				ID:                                     tt.args.id,
				Name:                                   tt.args.name,
				Description:                            tt.args.description,
				Org:                                    tt.args.org,
				InfrastructureProviderID:               tt.args.infrastructureProviderID,
				TenantID:                               tt.args.tenantID,
				SiteID:                                 tt.args.siteID,
				NetworkVirtualizationType:              tt.args.networkVirtualizationType,
				SlaacEnabled:                           tt.args.slaacEnabled,
				RoutingProfile:                         tt.args.routingProfile,
				RoutingProfileOverrides:                tt.args.routingProfileOverrides,
				ControllerVpcID:                        tt.args.controllerVpcID,
				Vni:                                    tt.args.vni,
				NetworkSecurityGroupID:                 tt.args.networkSecurityGroupID,
				NetworkSecurityGroupPropagationDetails: tt.args.networkSecurityGroupPropagationDetails,
				Labels:                                 tt.args.labels,
				Status:                                 tt.args.status,
				CreatedBy:                              tt.args.createdBy,
			}

			got, err := vsd.Create(tt.args.ctx, nil, input)
			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.Description, *got.Description)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, tt.want.InfrastructureProviderID, got.InfrastructureProviderID)
			assert.Equal(t, tt.want.TenantID, got.TenantID)
			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, *tt.want.NetworkVirtualizationType, *got.NetworkVirtualizationType)
			assert.Equal(t, tt.want.SlaacEnabled, got.SlaacEnabled)
			assert.Equal(t, len(tt.want.Labels), len(got.Labels))
			assert.Equal(t, *tt.want.ControllerVpcID, *got.ControllerVpcID)
			assert.Equal(t, tt.want.RoutingProfile, got.RoutingProfile)
			assert.Equal(t, tt.want.RoutingProfileOverrides, got.RoutingProfileOverrides)
			if tt.want.Vni != nil {
				assert.NotNil(t, got.Vni)
				assert.Equal(t, *tt.want.Vni, *got.Vni)
			}
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, *tt.want.NetworkSecurityGroupID, *got.NetworkSecurityGroupID)
			assert.True(t, proto.Equal(tt.want.NetworkSecurityGroupPropagationDetails, got.NetworkSecurityGroupPropagationDetails))
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)

			// A fresh read proves the JSONB override value was persisted.
			persisted, gerr := vsd.GetByID(tt.args.ctx, nil, got.ID, nil)
			require.NoError(t, gerr)
			assert.Equal(t, tt.want.SlaacEnabled, persisted.SlaacEnabled)
			assert.Equal(t, tt.want.RoutingProfileOverrides, persisted.RoutingProfileOverrides)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestVpcSQLDAO_Update(t *testing.T) {
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "test-provider-org", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn, st, "testNetworkSecurityGroup")
	networkSecurityGroup2 := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn, st, "testNetworkSecurityGroup2")

	vpc := testBuildVpc(t, dbSession, nil, "test-vpc", nil, tn.Org, ip.ID, tn.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, nil, tnu.ID, &networkSecurityGroup.ID)
	routingProfileOverrides := &VpcRoutingProfileOverrides{
		RouteTargetsOnExports:         &[]VpcRouteTarget{{ASN: 64513, VNI: 202}},
		TenantLeakCommunitiesAccepted: cutil.GetPtr(true),
	}
	effectiveRoutingProfile := &VpcEffectiveRoutingProfile{
		RouteTargetsOnExports:         []VpcRouteTarget{{ASN: 64513, VNI: 202}},
		TenantLeakCommunitiesAccepted: true,
		Internal:                      true,
		AccessTier:                    9,
	}

	uvpc := &Vpc{
		Name:                      "test-updated",
		Description:               cutil.GetPtr("Test Updated"),
		NetworkVirtualizationType: cutil.GetPtr(VpcEthernetVirtualizerWithNVUE),
		RoutingProfile:            cutil.GetPtr("EXTERNAL"),
		RoutingProfileOverrides:   routingProfileOverrides,
		EffectiveRoutingProfile:   effectiveRoutingProfile,
		NetworkSecurityGroupID:    &networkSecurityGroup2.ID,
		ControllerVpcID:           cutil.GetPtr(uuid.New()),
		ActiveVni:                 cutil.GetPtr(777),
		Vni:                       cutil.GetPtr(888),
		Status:                    VpcStatusReady,
		IsMissingOnSite:           true,
		SlaacEnabled:              true,
		Labels: map[string]string{
			"zone": "west1",
		},
		NetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{
			NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
				Id:                      "",
				RelatedInstanceIds:      []string{},
				UnpropagatedInstanceIds: []string{},
				Status:                  corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL,
			},
		},
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx                                    context.Context
		id                                     uuid.UUID
		name                                   *string
		description                            *string
		networkVirtualizationType              *string
		routingProfile                         *string
		routingProfileOverrides                *VpcRoutingProfileOverrides
		effectiveRoutingProfile                *VpcEffectiveRoutingProfile
		networkSecurityGroupID                 *string
		NetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails
		ControllervpcID                        *uuid.UUID
		ActiveVni                              *int
		Vni                                    *int
		labels                                 map[string]string
		Status                                 string
		IsMissingOnSite                        bool
		SlaacEnabled                           bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *Vpc
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update Vpc from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                                    ctx,
				id:                                     vpc.ID,
				name:                                   &uvpc.Name,
				description:                            uvpc.Description,
				networkVirtualizationType:              uvpc.NetworkVirtualizationType,
				routingProfile:                         uvpc.RoutingProfile,
				routingProfileOverrides:                uvpc.RoutingProfileOverrides,
				effectiveRoutingProfile:                uvpc.EffectiveRoutingProfile,
				networkSecurityGroupID:                 uvpc.NetworkSecurityGroupID,
				NetworkSecurityGroupPropagationDetails: uvpc.NetworkSecurityGroupPropagationDetails,
				ControllervpcID:                        uvpc.ControllerVpcID,
				ActiveVni:                              uvpc.ActiveVni,
				Vni:                                    uvpc.Vni,
				labels:                                 uvpc.Labels,
				Status:                                 uvpc.Status,
				IsMissingOnSite:                        uvpc.IsMissingOnSite,
				SlaacEnabled:                           uvpc.SlaacEnabled,
			},
			want:               uvpc,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vsd := VpcSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			input := VpcUpdateInput{
				VpcID:                                  tt.args.id,
				Name:                                   tt.args.name,
				Description:                            tt.args.description,
				NetworkVirtualizationType:              tt.args.networkVirtualizationType,
				RoutingProfile:                         tt.args.routingProfile,
				RoutingProfileOverrides:                tt.args.routingProfileOverrides,
				EffectiveRoutingProfile:                tt.args.effectiveRoutingProfile,
				NetworkSecurityGroupID:                 tt.args.networkSecurityGroupID,
				NetworkSecurityGroupPropagationDetails: tt.args.NetworkSecurityGroupPropagationDetails,
				ControllerVpcID:                        tt.args.ControllervpcID,
				ActiveVni:                              tt.args.ActiveVni,
				Vni:                                    tt.args.Vni,
				Labels:                                 tt.args.labels,
				Status:                                 &tt.args.Status,
				IsMissingOnSite:                        &tt.args.IsMissingOnSite,
				SlaacEnabled:                           &tt.args.SlaacEnabled,
			}

			got, err := vsd.Update(tt.args.ctx, nil, input)

			fmt.Printf("\ngot ID: %v, Created: %v, Updated: %v", got.ID.String(), got.Created, got.Updated)

			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.Description, *got.Description)
			assert.Equal(t, *tt.want.NetworkVirtualizationType, *got.NetworkVirtualizationType)
			assert.Equal(t, tt.want.SlaacEnabled, got.SlaacEnabled)
			assert.Equal(t, tt.want.RoutingProfile, got.RoutingProfile)
			assert.Equal(t, tt.want.RoutingProfileOverrides, got.RoutingProfileOverrides)
			assert.Equal(t, tt.want.EffectiveRoutingProfile, got.EffectiveRoutingProfile)
			assert.Equal(t, *tt.want.ControllerVpcID, *got.ControllerVpcID)
			assert.Equal(t, *tt.want.ActiveVni, *got.ActiveVni)
			assert.Equal(t, *tt.want.Vni, *got.Vni)

			if tt.want.NetworkSecurityGroupID != nil {
				assert.NotNil(t, got.NetworkSecurityGroupID)
				assert.Equal(t, *tt.want.NetworkSecurityGroupID, *got.NetworkSecurityGroupID)
			}

			if tt.args.NetworkSecurityGroupPropagationDetails != nil {
				assert.True(t, proto.Equal(tt.want.NetworkSecurityGroupPropagationDetails, got.NetworkSecurityGroupPropagationDetails))
			}
			assert.Equal(t, tt.want.Labels, got.Labels)
			assert.Equal(t, tt.want.Status, got.Status)

			// A fresh read verifies both JSONB columns, not only the update return value.
			persisted, gerr := vsd.GetByID(tt.args.ctx, nil, got.ID, nil)
			require.NoError(t, gerr)
			assert.Equal(t, tt.want.SlaacEnabled, persisted.SlaacEnabled)
			assert.Equal(t, tt.want.RoutingProfileOverrides, persisted.RoutingProfileOverrides)
			assert.Equal(t, tt.want.EffectiveRoutingProfile, persisted.EffectiveRoutingProfile)

			assert.NotEqualValues(t, got.Updated, vpc.Updated)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

	t.Run("persists an explicit SLAAC disable", func(t *testing.T) {
		vpcDAO := NewVpcDAO(dbSession)
		persisted, err := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
		require.NoError(t, err)
		require.True(t, persisted.SlaacEnabled)

		updated, err := vpcDAO.Update(ctx, nil, VpcUpdateInput{
			VpcID:        vpc.ID,
			SlaacEnabled: cutil.GetPtr(false),
		})
		require.NoError(t, err)
		assert.False(t, updated.SlaacEnabled)

		persisted, err = vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
		require.NoError(t, err)
		assert.False(t, persisted.SlaacEnabled)
	})

	t.Run("preserves an omitted effective routing profile", func(t *testing.T) {
		// Updating desired overrides alone must not clear cached controller state.
		replacementOverrides := &VpcRoutingProfileOverrides{
			AllowedAnycastPrefixes: &[]string{"192.0.2.0/24"},
		}
		updated, err := NewVpcDAO(dbSession).Update(ctx, nil, VpcUpdateInput{
			VpcID:                   vpc.ID,
			RoutingProfileOverrides: replacementOverrides,
		})
		require.NoError(t, err)
		assert.Equal(t, replacementOverrides, updated.RoutingProfileOverrides)
		assert.Equal(t, effectiveRoutingProfile, updated.EffectiveRoutingProfile)

		persisted, err := NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, replacementOverrides, persisted.RoutingProfileOverrides)
		assert.Equal(t, effectiveRoutingProfile, persisted.EffectiveRoutingProfile)
	})
}

func TestVpcSQLDAO_DeleteByID(t *testing.T) {
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
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn, st, "testNetworkSecurityGroup")

	vpc := testBuildVpc(t, dbSession, nil, "test-vpc", nil, tn.Org, ip.ID, tn.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), nil, nil, nil, tnu.ID, &networkSecurityGroup.ID)

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
			name: "delete Vpc by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  vpc.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vsd := VpcSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			err := vsd.DeleteByID(tt.args.ctx, nil, tt.args.id)
			require.Equal(t, tt.wantErr, err != nil)

			dvpc := &Vpc{}
			err = dbSession.DB.NewSelect().Model(dvpc).WhereDeleted().Where("id = ?", vpc.ID).Scan(context.Background())
			require.NoError(t, err)
			assert.NotNil(t, dvpc.Deleted)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestVpcSQLDAO_ClearDeleted(t *testing.T) {
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testVpcSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)
	controllerID := uuid.New()
	vpc := testBuildVpc(
		t,
		dbSession,
		nil,
		"test-vpc",
		nil,
		tn.Org,
		ip.ID,
		tn.ID,
		st.ID,
		nil,
		cutil.GetPtr(VpcEthernetVirtualizer),
		&controllerID,
		nil,
		cutil.GetPtr(VpcStatusError),
		tnu.ID,
		nil,
	)

	vpcDAO := NewVpcDAO(dbSession)
	require.NoError(t, vpcDAO.DeleteByID(context.Background(), nil, vpc.ID))

	t.Run("clears soft-delete marker", func(t *testing.T) {
		cleared, err := vpcDAO.Clear(context.Background(), nil, VpcClearInput{
			VpcID:   vpc.ID,
			Deleted: true,
		})
		require.NoError(t, err)
		require.NotNil(t, cleared)
		assert.Nil(t, cleared.Deleted)

		updated, err := vpcDAO.Update(context.Background(), nil, VpcUpdateInput{
			VpcID:           vpc.ID,
			Status:          cutil.GetPtr(VpcStatusReady),
			IsMissingOnSite: cutil.GetPtr(false),
		})
		require.NoError(t, err)
		assert.Equal(t, VpcStatusReady, updated.Status)
		assert.False(t, updated.IsMissingOnSite)
	})

	t.Run("clears soft-delete marker and description together", func(t *testing.T) {
		_, err := vpcDAO.Update(context.Background(), nil, VpcUpdateInput{
			VpcID:       vpc.ID,
			Description: cutil.GetPtr("description to clear"),
		})
		require.NoError(t, err)
		require.NoError(t, vpcDAO.DeleteByID(context.Background(), nil, vpc.ID))

		cleared, err := vpcDAO.Clear(context.Background(), nil, VpcClearInput{
			VpcID:       vpc.ID,
			Deleted:     true,
			Description: true,
		})
		require.NoError(t, err)
		require.NotNil(t, cleared)
		assert.Nil(t, cleared.Deleted)
		assert.Nil(t, cleared.Description)
	})

	t.Run("returns not found for unknown VPC", func(t *testing.T) {
		_, err := vpcDAO.Clear(context.Background(), nil, VpcClearInput{
			VpcID:   uuid.New(),
			Deleted: true,
		})
		assert.ErrorIs(t, err, db.ErrDoesNotExist)
	})
}

func TestVpcSQLDAO_ClearFromParams(t *testing.T) {
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testVpcSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "test-provider-org", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tn, st, "testNetworkSecurityGroup")

	vpc := testBuildVpc(t, dbSession, nil, "test-vpc", cutil.GetPtr("Test Description"), tn.Org, ip.ID, tn.ID, st.ID, nil, cutil.GetPtr(VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(VpcStatusReady), tnu.ID, &networkSecurityGroup.ID)
	vpc.NetworkSecurityGroupPropagationDetails = &NetworkSecurityGroupPropagationDetails{
		NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{},
	}
	vpc.RoutingProfileOverrides = &VpcRoutingProfileOverrides{
		LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
	}
	vpc.EffectiveRoutingProfile = &VpcEffectiveRoutingProfile{
		LeakDefaultRouteFromUnderlay: true,
		Internal:                     true,
		AccessTier:                   9,
	}

	testUpdateVpc(t, dbSession, vpc)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession  *db.Session
		tracerSpan *stracer.TracerSpan
	}
	type args struct {
		ctx                                    context.Context
		tx                                     *db.Tx
		id                                     uuid.UUID
		description                            bool
		controllerVpcID                        bool
		labels                                 bool
		networkSecuritygroupID                 bool
		networkSecurityGroupPropagationDetails bool
		routingProfileOverrides                bool
		effectiveRoutingProfile                bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "clearing VPC attributes returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                                    ctx,
				id:                                     vpc.ID,
				description:                            true,
				controllerVpcID:                        true,
				labels:                                 true,
				networkSecuritygroupID:                 true,
				networkSecurityGroupPropagationDetails: true,
				routingProfileOverrides:                true,
				effectiveRoutingProfile:                true,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vsd := VpcSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: tt.fields.tracerSpan,
			}

			input := VpcClearInput{
				VpcID:                                  tt.args.id,
				Description:                            tt.args.description,
				ControllerVpcID:                        tt.args.controllerVpcID,
				Labels:                                 tt.args.labels,
				NetworkSecurityGroupID:                 tt.args.networkSecuritygroupID,
				NetworkSecurityGroupPropagationDetails: tt.args.networkSecurityGroupPropagationDetails,
				RoutingProfileOverrides:                tt.args.routingProfileOverrides,
				EffectiveRoutingProfile:                tt.args.effectiveRoutingProfile,
			}

			got, err := vsd.Clear(tt.args.ctx, tt.args.tx, input)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}

			if tt.args.networkSecurityGroupPropagationDetails {
				assert.Nil(t, got.NetworkSecurityGroupPropagationDetails)
			}

			if tt.args.description {
				assert.Nil(t, got.Description)
			}

			if tt.args.controllerVpcID {
				assert.Nil(t, got.ControllerVpcID)
			}

			if tt.args.labels {
				assert.Nil(t, got.Labels)
			}

			if tt.args.networkSecuritygroupID {
				assert.Nil(t, got.NetworkSecurityGroupID)
				assert.Nil(t, got.NetworkSecurityGroup)
			}

			if tt.args.routingProfileOverrides {
				assert.Nil(t, got.RoutingProfileOverrides)
			}

			if tt.args.effectiveRoutingProfile {
				assert.Nil(t, got.EffectiveRoutingProfile)
			}

			// A fresh lookup verifies both JSONB columns were cleared in storage.
			persisted, gerr := vsd.GetByID(tt.args.ctx, nil, got.ID, nil)
			require.NoError(t, gerr)
			assert.Nil(t, persisted.RoutingProfileOverrides)
			assert.Nil(t, persisted.EffectiveRoutingProfile)
		})
	}
}

func TestVpc_GetSiteID(t *testing.T) {
	id := uuid.New()
	ctrlID := uuid.New()

	t.Run("falls back to ID when ControllerVpcID is nil", func(t *testing.T) {
		v := &Vpc{ID: id}
		got := v.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, id, *got)
	})

	t.Run("uses ControllerVpcID when set", func(t *testing.T) {
		v := &Vpc{ID: id, ControllerVpcID: &ctrlID}
		got := v.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, ctrlID, *got)
	})
}

func TestVpc_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"
	nsg := "nsg-1"
	nvllpID := uuid.New()

	t.Run("emits config and status without deprecated flat mirrors", func(t *testing.T) {
		fnn := VpcFNN
		routingProfile := "INTERNAL"
		requestedVni := 4242
		activeVni := 9999
		v := &Vpc{
			ID:                        id,
			Org:                       "org-1",
			Name:                      "vpc-a",
			Description:               &desc,
			NetworkSecurityGroupID:    &nsg,
			NVLinkLogicalPartitionID:  &nvllpID,
			NetworkVirtualizationType: &fnn,
			SlaacEnabled:              true,
			RoutingProfile:            &routingProfile,
			Vni:                       &requestedVni,
			ActiveVni:                 &activeVni,
			Labels:                    map[string]string{"env": "prod"},
		}
		got := v.ToProto()
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		assert.Equal(t, "vpc-a", got.Name)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "vpc-a", got.Metadata.Name)
		assert.Equal(t, "primary", got.Metadata.Description)
		require.Len(t, got.Metadata.Labels, 1)
		assert.Equal(t, "env", got.Metadata.Labels[0].Key)
		require.NotNil(t, got.Metadata.Labels[0].Value)
		assert.Equal(t, "prod", *got.Metadata.Labels[0].Value)

		// Desired configuration is emitted via the structured `config`.
		require.NotNil(t, got.Config)
		assert.Equal(t, "org-1", got.Config.TenantOrganizationId)
		require.NotNil(t, got.Config.NetworkSecurityGroupId)
		assert.Equal(t, "nsg-1", *got.Config.NetworkSecurityGroupId)
		require.NotNil(t, got.Config.DefaultNvlinkLogicalPartitionId)
		assert.Equal(t, nvllpID.String(), got.Config.DefaultNvlinkLogicalPartitionId.Value)
		require.NotNil(t, got.Config.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_FNN, *got.Config.NetworkVirtualizationType)
		require.NotNil(t, got.Config.SlaacEnabled)
		assert.True(t, *got.Config.SlaacEnabled)
		require.NotNil(t, got.Config.Vni)
		assert.Equal(t, uint32(requestedVni), *got.Config.Vni)
		require.NotNil(t, got.Config.RoutingProfileType)
		assert.Equal(t, routingProfile, *got.Config.RoutingProfileType)

		// The allocated VNI is emitted via `status`.
		require.NotNil(t, got.Status)
		require.NotNil(t, got.Status.Vni)
		assert.Equal(t, uint32(activeVni), *got.Status.Vni)
	})

	t.Run("nil description and labels yield zero-value metadata", func(t *testing.T) {
		v := &Vpc{ID: id, Org: "org-1", Name: "vpc-a"}
		got := v.ToProto()
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "", got.Metadata.Description)
		assert.Nil(t, got.Metadata.Labels)
		require.NotNil(t, got.Config)
		assert.Nil(t, got.Config.NetworkSecurityGroupId)
		assert.Nil(t, got.Config.DefaultNvlinkLogicalPartitionId)
		require.NotNil(t, got.Config.SlaacEnabled)
		assert.False(t, *got.Config.SlaacEnabled)
	})

	t.Run("uses ControllerVpcID for the proto Id when set", func(t *testing.T) {
		ctrlID := uuid.New()
		v := &Vpc{ID: id, ControllerVpcID: &ctrlID, Name: "vpc-a"}
		got := v.ToProto()
		require.NotNil(t, got.Id)
		assert.Equal(t, ctrlID.String(), got.Id.Value)
	})

	t.Run("explicit NSG clear preserves empty string (distinct from nil)", func(t *testing.T) {
		empty := ""
		v := &Vpc{ID: id, Name: "vpc-a", NetworkSecurityGroupID: &empty}
		got := v.ToProto()
		require.NotNil(t, got.Config)
		require.NotNil(t, got.Config.NetworkSecurityGroupId)
		assert.Equal(t, "", *got.Config.NetworkSecurityGroupId)
	})

	t.Run("maps NetworkVirtualizationType FNN string to the FNN enum", func(t *testing.T) {
		fnn := VpcFNN
		v := &Vpc{ID: id, Name: "vpc-a", NetworkVirtualizationType: &fnn}
		got := v.ToProto()
		require.NotNil(t, got.Config)
		require.NotNil(t, got.Config.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_FNN, *got.Config.NetworkVirtualizationType)
	})

	t.Run("maps NetworkVirtualizationType FLAT string to the FLAT enum", func(t *testing.T) {
		flat := VpcFlat
		v := &Vpc{ID: id, Name: "vpc-a", NetworkVirtualizationType: &flat}
		got := v.ToProto()
		require.NotNil(t, got.Config)
		require.NotNil(t, got.Config.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_FLAT, *got.Config.NetworkVirtualizationType)
	})

	t.Run("maps NetworkVirtualizationType ethernet string to ETHERNET_VIRTUALIZER", func(t *testing.T) {
		eth := VpcEthernetVirtualizer
		v := &Vpc{ID: id, Name: "vpc-a", NetworkVirtualizationType: &eth}
		got := v.ToProto()
		require.NotNil(t, got.Config)
		require.NotNil(t, got.Config.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_ETHERNET_VIRTUALIZER, *got.Config.NetworkVirtualizationType)
	})

	t.Run("omits NetworkVirtualizationType when the entity has none", func(t *testing.T) {
		v := &Vpc{ID: id, Name: "vpc-a"}
		got := v.ToProto()
		require.NotNil(t, got.Config)
		assert.Nil(t, got.Config.NetworkVirtualizationType)
	})

	t.Run("defaults an unrecognized NetworkVirtualizationType to ETHERNET_VIRTUALIZER", func(t *testing.T) {
		unknown := "unknown"
		v := &Vpc{ID: id, Name: "vpc-a", NetworkVirtualizationType: &unknown}
		got := v.ToProto()
		require.NotNil(t, got.Config)
		require.NotNil(t, got.Config.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_ETHERNET_VIRTUALIZER, *got.Config.NetworkVirtualizationType)
	})
}

func TestVpc_FromProto(t *testing.T) {
	id := uuid.New()
	nvllpID := uuid.New()
	nsg := "nsg-1"

	t.Run("nil proto leaves receiver unchanged", func(t *testing.T) {
		v := &Vpc{ID: id, Name: "preserved", Org: "org-1"}
		v.FromProto(nil)
		assert.Equal(t, id, v.ID)
		assert.Equal(t, "preserved", v.Name)
		assert.Equal(t, "org-1", v.Org)
	})

	t.Run("invalid id leaves vpc.ID unchanged", func(t *testing.T) {
		v := &Vpc{ID: id}
		v.FromProto(&corev1.Vpc{
			Id:   &corev1.VpcId{Value: "not-a-uuid"},
			Name: "vpc-a",
		})
		assert.Equal(t, id, v.ID)
		assert.Equal(t, "vpc-a", v.Name)
	})

	t.Run("populates fields from proto", func(t *testing.T) {
		fnnEnum := corev1.VpcVirtualizationType_FNN
		requestedVni := uint32(12001)
		activeVni := uint32(12002)
		routingProfile := "INTERNAL"

		v := &Vpc{}
		v.FromProto(&corev1.Vpc{
			Id:   &corev1.VpcId{Value: id.String()},
			Name: "vpc-a",
			Config: &corev1.VpcConfig{
				TenantOrganizationId:            "org-1",
				NetworkSecurityGroupId:          &nsg,
				NetworkVirtualizationType:       &fnnEnum,
				SlaacEnabled:                    cutil.GetPtr(true),
				Vni:                             &requestedVni,
				RoutingProfileType:              &routingProfile,
				DefaultNvlinkLogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllpID.String()},
			},
			Status: &corev1.VpcStatus{Vni: &activeVni},
			Metadata: &corev1.Metadata{
				Name:        "vpc-a",
				Description: "primary",
				Labels: []*corev1.Label{
					{Key: "env", Value: cutil.GetPtr("prod")},
				},
			},
		})
		assert.Equal(t, id, v.ID)
		assert.Equal(t, "vpc-a", v.Name)
		assert.Equal(t, "org-1", v.Org)
		require.NotNil(t, v.NetworkSecurityGroupID)
		assert.Equal(t, "nsg-1", *v.NetworkSecurityGroupID)
		require.NotNil(t, v.NetworkVirtualizationType)
		assert.Equal(t, VpcFNN, *v.NetworkVirtualizationType)
		assert.True(t, v.SlaacEnabled)
		require.NotNil(t, v.Vni)
		assert.Equal(t, int(requestedVni), *v.Vni)
		require.NotNil(t, v.ActiveVni)
		assert.Equal(t, int(activeVni), *v.ActiveVni)
		require.NotNil(t, v.RoutingProfile)
		assert.Equal(t, routingProfile, *v.RoutingProfile)
		require.NotNil(t, v.NVLinkLogicalPartitionID)
		assert.Equal(t, nvllpID, *v.NVLinkLogicalPartitionID)
		require.NotNil(t, v.Description)
		assert.Equal(t, "primary", *v.Description)
		assert.Equal(t, Labels{"env": "prod"}, v.Labels)
	})

	t.Run("maps proto network virtualization types", func(t *testing.T) {
		cases := []struct {
			name string
			in   corev1.VpcVirtualizationType
			want string
		}{
			{name: "FLAT", in: corev1.VpcVirtualizationType_FLAT, want: VpcFlat},
			{name: "unhandled defaults to ETHERNET_VIRTUALIZER", in: corev1.VpcVirtualizationType_FNN_CLASSIC, want: VpcEthernetVirtualizer},
		}
		for _, tc := range cases {
			t.Run(tc.name, func(t *testing.T) {
				v := &Vpc{}
				v.FromProto(&corev1.Vpc{Config: &corev1.VpcConfig{NetworkVirtualizationType: &tc.in}})
				require.NotNil(t, v.NetworkVirtualizationType)
				assert.Equal(t, tc.want, *v.NetworkVirtualizationType)
			})
		}
	})

	t.Run("clears stale fields when structured fields are absent", func(t *testing.T) {
		staleNvllp := uuid.New()
		staleNSG := "stale-nsg"
		staleVirt := VpcFNN
		staleRouting := "INTERNAL"
		staleRequested := 7000
		staleActive := 7001
		staleDesc := "stale"

		v := &Vpc{
			ID:                        id,
			Org:                       "stale-org",
			Description:               &staleDesc,
			NetworkSecurityGroupID:    &staleNSG,
			NetworkVirtualizationType: &staleVirt,
			SlaacEnabled:              true,
			RoutingProfile:            &staleRouting,
			Vni:                       &staleRequested,
			ActiveVni:                 &staleActive,
			NVLinkLogicalPartitionID:  &staleNvllp,
			RoutingProfileOverrides:   &VpcRoutingProfileOverrides{LeakDefaultRouteFromUnderlay: cutil.GetPtr(false)},
			EffectiveRoutingProfile:   &VpcEffectiveRoutingProfile{Internal: true, AccessTier: 4},
			Labels:                    map[string]string{"old": "val"},
		}
		v.FromProto(&corev1.Vpc{
			Id:       &corev1.VpcId{Value: id.String()},
			Metadata: &corev1.Metadata{Name: "reset"},
		})

		assert.Equal(t, "reset", v.Name)
		assert.Empty(t, v.Org)
		assert.Nil(t, v.NetworkSecurityGroupID)
		assert.Nil(t, v.NetworkVirtualizationType)
		assert.False(t, v.SlaacEnabled)
		assert.Nil(t, v.RoutingProfile)
		assert.Nil(t, v.Vni)
		assert.Nil(t, v.ActiveVni)
		assert.Nil(t, v.NVLinkLogicalPartitionID)
		assert.Nil(t, v.RoutingProfileOverrides)
		assert.Nil(t, v.EffectiveRoutingProfile)
		assert.Nil(t, v.Description)
		assert.Nil(t, v.Labels)
	})

	t.Run("invalid NVLink partition id clears the field", func(t *testing.T) {
		staleNvllp := uuid.New()
		v := &Vpc{ID: id, NVLinkLogicalPartitionID: &staleNvllp}
		v.FromProto(&corev1.Vpc{
			Id:   &corev1.VpcId{Value: id.String()},
			Name: "vpc-a",
			Config: &corev1.VpcConfig{
				DefaultNvlinkLogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: "not-a-uuid"},
			},
		})
		assert.Nil(t, v.NVLinkLogicalPartitionID)
	})

	t.Run("prefers Metadata.Name over the deprecated top-level Name field", func(t *testing.T) {
		v := &Vpc{}
		v.FromProto(&corev1.Vpc{
			Id:       &corev1.VpcId{Value: id.String()},
			Name:     "deprecated-top-level",
			Metadata: &corev1.Metadata{Name: "metadata-name"},
		})
		assert.Equal(t, "metadata-name", v.Name)
	})

	t.Run("falls back to top-level Name when Metadata.Name is empty", func(t *testing.T) {
		v := &Vpc{}
		v.FromProto(&corev1.Vpc{
			Id:       &corev1.VpcId{Value: id.String()},
			Name:     "top-level-fallback",
			Metadata: &corev1.Metadata{Name: ""},
		})
		assert.Equal(t, "top-level-fallback", v.Name)
	})
}

// TestVpc_ToProtoFromProto_RoundTrip verifies the entity survives a
// ToProto -> FromProto round trip. ID round-trips cleanly only when
// ControllerVpcID is unset (ToProto sources proto.Id from GetSiteID).
func TestVpc_ToProtoFromProto_RoundTrip(t *testing.T) {
	id := uuid.New()
	nvllpID := uuid.New()
	fnn := VpcFNN
	nsg := "nsg-rt"
	routing := "INTERNAL"
	requested := 20001
	active := 20002

	orig := &Vpc{
		ID:                        id,
		Org:                       "org-rt",
		Name:                      "vpc-rt",
		NetworkVirtualizationType: &fnn,
		SlaacEnabled:              true,
		NetworkSecurityGroupID:    &nsg,
		RoutingProfile:            &routing,
		Vni:                       &requested,
		ActiveVni:                 &active,
		NVLinkLogicalPartitionID:  &nvllpID,
		RoutingProfileOverrides: &VpcRoutingProfileOverrides{
			RouteTargetImports:           &[]VpcRouteTarget{{ASN: 64512, VNI: 23}},
			RouteTargetsOnExports:        &[]VpcRouteTarget{},
			LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
			AcceptedLeaksFromUnderlay:    &[]string{"10.0.0.1/24"},
			AllowedAnycastPrefixes:       &[]string{},
		},
		EffectiveRoutingProfile: &VpcEffectiveRoutingProfile{
			RouteTargetImports:             []VpcRouteTarget{{ASN: 64512, VNI: 23}},
			RouteTargetsOnExports:          []VpcRouteTarget{},
			LeakDefaultRouteFromUnderlay:   true,
			LeakTenantHostRoutesToUnderlay: true,
			AcceptedLeaksFromUnderlay:      []string{"10.0.0.0/24"},
			AllowedAnycastPrefixes:         []string{},
			Internal:                       true,
			AccessTier:                     4,
		},
	}

	got := &Vpc{}
	got.FromProto(orig.ToProto())

	assert.Equal(t, orig.ID, got.ID)
	assert.Equal(t, orig.Name, got.Name)
	assert.Equal(t, orig.Org, got.Org)
	assert.Equal(t, orig.NetworkVirtualizationType, got.NetworkVirtualizationType)
	assert.Equal(t, orig.SlaacEnabled, got.SlaacEnabled)
	assert.Equal(t, orig.NetworkSecurityGroupID, got.NetworkSecurityGroupID)
	assert.Equal(t, orig.RoutingProfile, got.RoutingProfile)
	assert.Equal(t, orig.Vni, got.Vni)
	assert.Equal(t, orig.ActiveVni, got.ActiveVni)
	assert.Equal(t, orig.NVLinkLogicalPartitionID, got.NVLinkLogicalPartitionID)
	assert.Equal(t, orig.RoutingProfileOverrides, got.RoutingProfileOverrides)
	assert.Equal(t, orig.EffectiveRoutingProfile, got.EffectiveRoutingProfile)

	t.Run("normalizes nil effective routing profile lists", func(t *testing.T) {
		// Core repeated fields canonicalize nil lists to allocated empty slices.
		profileVpc := &Vpc{
			ID:   uuid.New(),
			Name: "vpc-a",
			EffectiveRoutingProfile: &VpcEffectiveRoutingProfile{
				LeakDefaultRouteFromUnderlay:   true,
				LeakTenantHostRoutesToUnderlay: true,
				TenantLeakCommunitiesAccepted:  true,
				Internal:                       true,
				AccessTier:                     4,
			},
		}

		roundTripped := &Vpc{}
		roundTripped.FromProto(profileVpc.ToProto())
		require.NotNil(t, roundTripped.EffectiveRoutingProfile)
		assert.NotNil(t, roundTripped.EffectiveRoutingProfile.RouteTargetImports)
		assert.Empty(t, roundTripped.EffectiveRoutingProfile.RouteTargetImports)
		assert.NotNil(t, roundTripped.EffectiveRoutingProfile.RouteTargetsOnExports)
		assert.Empty(t, roundTripped.EffectiveRoutingProfile.RouteTargetsOnExports)
		assert.NotNil(t, roundTripped.EffectiveRoutingProfile.AcceptedLeaksFromUnderlay)
		assert.Empty(t, roundTripped.EffectiveRoutingProfile.AcceptedLeaksFromUnderlay)
		assert.NotNil(t, roundTripped.EffectiveRoutingProfile.AllowedAnycastPrefixes)
		assert.Empty(t, roundTripped.EffectiveRoutingProfile.AllowedAnycastPrefixes)
		assert.True(t, roundTripped.EffectiveRoutingProfile.LeakDefaultRouteFromUnderlay)
		assert.True(t, roundTripped.EffectiveRoutingProfile.LeakTenantHostRoutesToUnderlay)
		assert.True(t, roundTripped.EffectiveRoutingProfile.TenantLeakCommunitiesAccepted)
		assert.True(t, roundTripped.EffectiveRoutingProfile.Internal)
		assert.Equal(t, uint32(4), roundTripped.EffectiveRoutingProfile.AccessTier)
	})
}
