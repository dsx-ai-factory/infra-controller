// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestGetAllInfiniBandInterface_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqInstance              *cdbm.Instance
		reqInstanceID            string
		reqInfiniBandPartition   *cdbm.InfiniBandPartition
		reqInfiniBandPartitionID string
		reqSiteID                string
		reqOrg                   string
		reqUser                  *cdbm.User
		reqMachine               *cdbm.Machine
		respCode                 int
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	ibPartitions := []*cdbm.InfiniBandPartition{}
	for i := 0; i < 3; i++ {
		ibPartition := testBuildIBPartition(t, dbSession, fmt.Sprintf("test-InfiniBandPartition-%d", i), tn1.Org, st1, tn1, nil, cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
		assert.NotNil(t, ibPartition)
		ibPartitions = append(ibPartitions, ibPartition)
	}

	ibifcs := []*cdbm.InfiniBandInterface{}
	for i := 0; i < 25; i++ {
		ibPartition := ibPartitions[i%3]
		ibifc := testInstanceBuildIBInterface(t, dbSession, inst1, st1, ibPartition, i%4, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusProvisioning), false)
		assert.NotNil(t, ibifc)
		ibifcs = append(ibifcs, ibifc)
	}

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                          string
		fields                        fields
		args                          args
		wantErr                       bool
		queryStatus                   *string
		queryIncludeRelations1        *string
		queryIncludeRelations2        *string
		pageNumber                    *int
		pageSize                      *int
		orderBy                       *string
		expectedInfiniBandPartitionID *uuid.UUID
		expectedInstance              *cdbm.Instance
		expectedCount                 int
		expectedTotal                 int
		verifyChildSpanner            bool
	}{
		{
			name: "test InfiniBandInterface getall by Instance API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqSiteID:     st1.ID.String(),
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:            false,
			orderBy:            cutil.GetPtr("CREATED_ASC"),
			expectedCount:      20,
			expectedTotal:      25,
			expectedInstance:   inst1,
			verifyChildSpanner: true,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInfiniBandPartition:   ibPartitions[0],
				reqInfiniBandPartitionID: ibPartitions[0].ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
			},
			wantErr:                       false,
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 9,
			expectedTotal:                 9,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[0].ID),
			verifyChildSpanner:            true,
		},
		{
			name: "test InfiniBandInterface getall by Instance success with paging",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 10,
			expectedTotal:                 25,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[0].ID),
		},
		{
			name: "test InfiniBandInterface getall success with paging",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInfiniBandPartition:   ibPartitions[0],
				reqInfiniBandPartitionID: ibPartitions[0].ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 9,
			expectedTotal:                 9,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[0].ID),
		},
		{
			name: "test InfiniBandInterface getall by Instance success with paging on page 2",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(2),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 10,
			expectedTotal:                 25,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[1].ID),
		},
		{
			name: "test InfiniBandInterface getall success with paging on page 2",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInfiniBandPartition:   ibPartitions[1],
				reqInfiniBandPartitionID: ibPartitions[1].ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(2),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 0,
			expectedTotal:                 8,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[1].ID),
		},
		{
			name: "test InfiniBandInterface getall by Instance filter  with paging bad orderby",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			wantErr:    false,
			pageNumber: cutil.GetPtr(2),
			pageSize:   cutil.GetPtr(10),
			orderBy:    cutil.GetPtr("TEST_ASC"),
		},
		{
			name: "test InfiniBandInterface getall by Instance filter, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqOrg:      ipOrg,
				reqUser:     ipu,
				respCode:    http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test InfiniBandInterface getall by Instance filter, invalid Instance ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstanceID: "badID",
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test InfiniBandInterface getall by Instance filter, Instance ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   nil,
				reqInstanceID: uuid.New().String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition filter, InfiniBandPartition ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInfiniBandPartitionID: uuid.New().String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test InfiniBandInterface getall by Instance filter, Instance not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg2,
				reqUser:       tnu2,
				respCode:      http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test InfiniBandInterface getall by Instance filter success include relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			queryIncludeRelations1:        cutil.GetPtr(cdbm.InfiniBandPartitionRelationName),
			queryIncludeRelations2:        cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:                 20,
			expectedTotal:                 25,
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[0].ID),
			wantErr:                       false,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition filter include relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInfiniBandPartition:   ibPartitions[0],
				reqInfiniBandPartitionID: ibPartitions[0].ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
			},
			queryIncludeRelations1:        cutil.GetPtr(cdbm.InfiniBandPartitionRelationName),
			queryIncludeRelations2:        cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:                 9,
			expectedTotal:                 9,
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[0].ID),
			expectedInstance:              inst1,
			wantErr:                       false,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandInterfaceStatusProvisioning status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			queryStatus:                   cutil.GetPtr(cdbm.InfiniBandInterfaceStatusProvisioning),
			expectedCount:                 20,
			expectedTotal:                 25,
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedInfiniBandPartitionID: cutil.GetPtr(ibPartitions[0].ID),
			wantErr:                       false,
		},
		{
			name: "test InfiniBandInterface getall by BadStatus status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			queryStatus:   cutil.GetPtr("BadStatus"),
			expectedCount: 0,
			expectedTotal: 0,
			wantErr:       false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetAllInfiniBandInterfaceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			if tt.args.reqSiteID != "" {
				q.Add("siteId", tt.args.reqSiteID)
			}
			if tt.args.reqInstanceID != "" {
				q.Add("instanceId", tt.args.reqInstanceID)
			}
			if tt.args.reqInfiniBandPartitionID != "" {
				q.Add("infinibandPartitionId", tt.args.reqInfiniBandPartitionID)
			}
			if tt.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations1)
			}
			if tt.queryStatus != nil {
				q.Add("status", *tt.queryStatus)
			}
			if tt.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations2)
			}
			if tt.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tt.pageNumber))
			}
			if tt.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tt.pageSize))
			}
			if tt.orderBy != nil {
				q.Set("orderBy", *tt.orderBy)
			}
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/infiniband-interface", tt.args.reqOrg))
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.reqOrg)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetAllInfiniBandInterfaceByInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetAllInfiniBandInterfaceByInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := []model.APIInfiniBandInterface{}
			serr := json.Unmarshal(rec.Body.Bytes(), &rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, tt.expectedCount, len(rst))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err := json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			assert.Equal(t, tt.expectedTotal, pr.Total)

			if tt.queryIncludeRelations1 != nil || tt.queryIncludeRelations2 != nil {
				if tt.expectedInfiniBandPartitionID != nil && tt.expectedInfiniBandPartitionID.String() != "" {
					assert.Equal(t, tt.expectedInfiniBandPartitionID.String(), rst[0].InfiniBandPartition.ID)
				}
				if tt.expectedInstance != nil && tt.expectedInstance.ID.String() != "" {
					assert.Equal(t, tt.expectedInstance.ID.String(), rst[0].InstanceID)
				}
			} else {
				if len(rst) > 0 {
					if tt.expectedInstance != nil && tt.expectedInstance.ID.String() != "" {
						assert.Equal(t, tt.expectedInstance.ID.String(), rst[0].InstanceID)
					}
					if tt.expectedInfiniBandPartitionID != nil && tt.expectedInfiniBandPartitionID.String() != "" {
						assert.Equal(t, tt.expectedInfiniBandPartitionID.String(), rst[0].InfiniBandPartitonID)
					}
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNewGetAllInfiniBandInterfaceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetAllInfiniBandInterfaceHandler
	}{
		{
			name: "test GetAllInfiniBandInterfaceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllInfiniBandInterfaceHandler{
				dbSession:     dbSession,
				tc:            tc,
				cfg:           cfg,
				queryOverride: nil,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewGetAllInfiniBandInterfaceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg)
			if got.dbSession != tt.want.dbSession || got.tc != tt.want.tc || got.cfg != tt.want.cfg || got.queryOverride != nil {
				t.Errorf("NewGetAllInfiniBandInterfaceHandler() dbSession=%v tc=%v cfg=%v queryOverride=%v, want queryOverride=nil", got.dbSession, got.tc, got.cfg, got.queryOverride)
			}
		})
	}
}

func TestNewGetAllInstanceInfiniBandInterfaceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetAllInstanceInfiniBandInterfaceHandler
	}{
		{
			name: "test GetAllInstanceInfiniBandInterfaceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllInstanceInfiniBandInterfaceHandler{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllInstanceInfiniBandInterfaceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetAllInstanceInfiniBandInterfaceHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestGetAllInstanceInfiniBandInterfaceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqInstance   *cdbm.Instance
		reqInstanceID string
		reqOrg        string
		reqUser       *cdbm.User
		reqMachine    *cdbm.Machine
		respCode      int
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tn2.ID, st1.ID, tnu2.ID)
	assert.NotNil(t, ts2)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	subnets := []*cdbm.Subnet{}
	for i := 11; i <= 35; i++ {
		subnet1 := testInstanceBuildSubnet(t, dbSession, fmt.Sprintf("test-subnet-%d", i), tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
		assert.NotNil(t, subnet1)
		subnets = append(subnets, subnet1)
	}

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnets[0].ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	ifcs := []*cdbm.Interface{}
	for i := 0; i < 25; i++ {
		instsub1 := testInstanceBuildInstanceInterface(t, dbSession, inst1.ID, &subnets[i].ID, nil, nil, cdbm.InterfaceStatusProvisioning)
		assert.NotNil(t, instsub1)
		ifcs = append(ifcs, instsub1)
	}

	ibps := []*cdbm.InfiniBandPartition{}
	for i := 0; i < 25; i++ {
		ibp1 := testBuildIBPartition(t, dbSession, "test-infiniband-partition-1", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
		assert.NotNil(t, ibp1)
		ibps = append(ibps, ibp1)
	}

	ibifs := []*cdbm.InfiniBandInterface{}
	for i := 0; i < 25; i++ {
		ibif1 := testInstanceBuildIBInterface(t, dbSession, inst1, st1, ibps[i], i%4, true, cutil.GetPtr(1), cutil.GetPtr(cdbm.InfiniBandInterfaceStatusProvisioning), false)
		assert.NotNil(t, ibif1)
		ibifs = append(ibifs, ibif1)
	}

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                          string
		fields                        fields
		args                          args
		wantErr                       bool
		queryStatus                   *string
		queryIncludeRelations1        *string
		queryIncludeRelations2        *string
		pageNumber                    *int
		pageSize                      *int
		orderBy                       *string
		expectSubnet                  bool
		expectedInfiniBandPartitionID *uuid.UUID
		expectedInstanceID            *uuid.UUID
		expectedCount                 int
		expectedTotal                 int
		verifyChildSpanner            bool
		expectedErrorMessage          string
	}{
		{
			name: "test InfiniBandInterface getall by Instance API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:            false,
			orderBy:            cutil.GetPtr("CREATED_ASC"),
			expectedCount:      20,
			expectedTotal:      25,
			expectedInstanceID: cutil.GetPtr(inst1.ID),
			verifyChildSpanner: true,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 10,
			expectedTotal:                 25,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibps[0].ID),
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition success with paging on page 2",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(2),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectedCount:                 10,
			expectedTotal:                 25,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibps[10].ID),
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition error with paging bad orderby",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			wantErr:                       false,
			pageNumber:                    cutil.GetPtr(2),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("TEST_ASC"),
			expectedInfiniBandPartitionID: cutil.GetPtr(ibps[0].ID),
			expectedErrorMessage:          "Failed to validate pagination request data",
		},
		{
			name: "test InfiniBandInterface getall by Instance API failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        ipOrg,
				reqUser:       ipu,
				respCode:      http.StatusForbidden,
			},
			wantErr:              false,
			expectedErrorMessage: "User does not have Tenant Admin role with org",
		},
		{
			name: "test InfiniBandInterface getall by Instance API failure, invalid Instance ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: "badID",
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			wantErr:              false,
			expectedErrorMessage: "Invalid Instance ID: badID",
		},
		{
			name: "test InfiniBandInterface getall by Instance API failure, Instance ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: uuid.New().String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusNotFound,
			},
			wantErr:              false,
			expectedErrorMessage: "Could not find Instance with ID",
		},
		{
			name: "test InfiniBandInterface getall by Instance API failure, Instance not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg2,
				reqUser:       tnu2,
				respCode:      http.StatusForbidden,
			},
			wantErr:              false,
			expectedErrorMessage: "doesn't belong to current Tenant",
		},
		{
			name: "test InfiniBandInterface getall by Instance API endpoint success include relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			queryIncludeRelations1: cutil.GetPtr(cdbm.InfiniBandPartitionRelationName),
			queryIncludeRelations2: cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:          20,
			expectedTotal:          25,
			orderBy:                cutil.GetPtr("CREATED_ASC"),
			expectSubnet:           true,
			expectedInstanceID:     cutil.GetPtr(inst1.ID),
			wantErr:                false,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition InterfaceStatusProvisioning status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			queryStatus:                   cutil.GetPtr(cdbm.InterfaceStatusProvisioning),
			expectedCount:                 20,
			expectedTotal:                 25,
			orderBy:                       cutil.GetPtr("CREATED_ASC"),
			expectSubnet:                  true,
			expectedInfiniBandPartitionID: cutil.GetPtr(ibps[0].ID),
			wantErr:                       false,
		},
		{
			name: "test InfiniBandInterface getall by InfiniBandPartition BadStatus status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			queryStatus:   cutil.GetPtr("BadStatus"),
			expectedCount: 0,
			expectedTotal: 0,
			wantErr:       false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetAllInstanceInfiniBandInterfaceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			if tt.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations1)
			}
			if tt.queryStatus != nil {
				q.Add("status", *tt.queryStatus)
			}
			if tt.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations2)
			}
			if tt.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tt.pageNumber))
			}
			if tt.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tt.pageSize))
			}
			if tt.orderBy != nil {
				q.Set("orderBy", *tt.orderBy)
			}
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v/infinibandinterface", tt.args.reqOrg, tt.args.reqInstanceID))
			ec.SetParamNames("orgName", "instanceId")
			if tt.args.reqInstanceID != "" {
				ec.SetParamValues(tt.args.reqOrg, tt.args.reqInstanceID)
			}
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetAllInstanceInfiniBandInterfaceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetAllInstanceInfiniBandInterfaceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				if tt.expectedErrorMessage != "" {
					var errResp struct {
						Message string `json:"message"`
					}
					_ = json.Unmarshal(rec.Body.Bytes(), &errResp)
					assert.Contains(t, errResp.Message, tt.expectedErrorMessage)
				}
				return
			}

			rst := []model.APIInfiniBandInterface{}
			serr := json.Unmarshal(rec.Body.Bytes(), &rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, tt.expectedCount, len(rst))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err := json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			assert.Equal(t, tt.expectedTotal, pr.Total)

			if len(rst) > 0 {
				assert.Equal(t, rst[0].InstanceID, tt.args.reqInstance.ID.String())
				if tt.expectedInstanceID != nil {
					assert.Equal(t, tt.expectedInstanceID.String(), rst[0].InstanceID)
				}
				if tt.expectedInfiniBandPartitionID != nil {
					assert.Equal(t, tt.expectedInfiniBandPartitionID.String(), rst[0].InfiniBandPartitonID)
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
