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

func TestGetAllNVLinkInterface_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqInstance                 *cdbm.Instance
		reqInstanceID               string
		reqNvlinkLogicalPartition   *cdbm.NVLinkLogicalPartition
		reqNvlinkLogicalPartitionID string
		reqSiteID                   string
		reqNVLinkDomainID           *uuid.UUID
		reqOrg                      string
		reqUser                     *cdbm.User
		reqMachine                  *cdbm.Machine
		respCode                    int
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

	nvlinklogicalpartitions := []*cdbm.NVLinkLogicalPartition{}
	for i := 0; i < 3; i++ {
		nvlinklogicalpartition1 := testBuildNVLinkLogicalPartition(t, dbSession, fmt.Sprintf("test-nvlinklogicalpartition-%d", i), cutil.GetPtr("Test NVLink Logical Partition"), tn1.Org, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
		assert.NotNil(t, nvlinklogicalpartition1)
		nvlinklogicalpartitions = append(nvlinklogicalpartitions, nvlinklogicalpartition1)
	}

	nvlifcs := []*cdbm.NVLinkInterface{}
	for i := 0; i < 25; i++ {
		nvlinklogicalpartition := nvlinklogicalpartitions[i%3]
		nvlifc := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st1.ID, inst1.ID, nvlinklogicalpartition.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), i%4, cdbm.NVLinkInterfaceStatusProvisioning)
		assert.NotNil(t, nvlifc)
		nvlifcs = append(nvlifcs, nvlifc)
	}

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                             string
		fields                           fields
		args                             args
		wantErr                          bool
		queryStatus                      *string
		queryIncludeRelations1           *string
		queryIncludeRelations2           *string
		pageNumber                       *int
		pageSize                         *int
		orderBy                          *string
		expectedNVLinkLogicalPartitionID *uuid.UUID
		expectedDeviceInstance           *int
		expectedInstance                 *cdbm.Instance
		expectedNVLinkDomainID           *uuid.UUID
		expectedCount                    int
		expectedTotal                    int
		verifyChildSpanner               bool
	}{
		{
			name: "test NVLinkInterface getall by Instance API endpoint success",
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
			wantErr:                false,
			orderBy:                cutil.GetPtr("CREATED_ASC"),
			expectedCount:          20,
			expectedTotal:          25,
			expectedInstance:       inst1,
			expectedDeviceInstance: cutil.GetPtr(nvlifcs[0].DeviceInstance),
			verifyChildSpanner:     true,
		},
		{
			name: "test NVLinkInterface getall by NVLinkLogicalPartition API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqNvlinkLogicalPartition:   nvlinklogicalpartitions[0],
				reqNvlinkLogicalPartitionID: nvlinklogicalpartitions[0].ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			wantErr:                          false,
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    9,
			expectedTotal:                    9,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[0].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[0].DeviceInstance),
			verifyChildSpanner:               true,
		},
		{
			name: "test NVLinkInterface getall by NVLinkDomain API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqNVLinkDomainID: nvlifcs[0].NVLinkDomainID,
				reqOrg:            tnOrg1,
				reqUser:           tnu1,
				respCode:          http.StatusOK,
			},
			wantErr:                false,
			orderBy:                cutil.GetPtr("CREATED_ASC"),
			expectedCount:          1,
			expectedTotal:          1,
			expectedNVLinkDomainID: nvlifcs[0].NVLinkDomainID,
			expectedDeviceInstance: &nvlifcs[0].DeviceInstance,
			verifyChildSpanner:     true,
		},
		{
			name: "test NVLinkInterface getall by Instance success with paging",
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
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(1),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    10,
			expectedTotal:                    25,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[0].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[0].DeviceInstance),
		},
		{
			name: "test NVLinkInterface getall success with paging",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqNvlinkLogicalPartition:   nvlinklogicalpartitions[0],
				reqNvlinkLogicalPartitionID: nvlinklogicalpartitions[0].ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(1),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    9,
			expectedTotal:                    9,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[0].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[0].DeviceInstance),
		},
		{
			name: "test NVLinkInterface getall by Instance success with paging on page 2",
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
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(2),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    10,
			expectedTotal:                    25,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[1].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[10].DeviceInstance),
		},
		{
			name: "test NVLinkInterface getall success with paging on page 2",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqNvlinkLogicalPartition:   nvlinklogicalpartitions[1],
				reqNvlinkLogicalPartitionID: nvlinklogicalpartitions[1].ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(2),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    0,
			expectedTotal:                    8,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[1].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[10].DeviceInstance),
		},
		{
			name: "test NVLinkInterface getall by Instance filter  with paging bad orderby",
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
			name: "test NVLinkInterface getall by Instance filter, org does not have a Tenant associated",
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
			name: "test NVLinkInterface getall by Instance filter, invalid Instance ID in request",
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
			name: "test NVLinkInterface getall by Instance filter, Instance ID in request not found",
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
			name: "test NVLinkInterface getall by NVLinkLogicalPartition filter, NVLinkLogicalPartition ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqNvlinkLogicalPartitionID: uuid.New().String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test NVLinkInterface getall by Instance filter, Instance not belong to current tenant",
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
			name: "test NVLinkInterface getall by Instance filter success include relation",
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
			queryIncludeRelations1:           cutil.GetPtr(cdbm.NVLinkLogicalPartitionRelationName),
			queryIncludeRelations2:           cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:                    20,
			expectedTotal:                    25,
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[0].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[0].DeviceInstance),
			wantErr:                          false,
		},
		{
			name: "test NVLinkInterface getall by NVLinkLogicalPartition filter include relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqNvlinkLogicalPartition:   nvlinklogicalpartitions[0],
				reqNvlinkLogicalPartitionID: nvlinklogicalpartitions[0].ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			queryIncludeRelations1:           cutil.GetPtr(cdbm.NVLinkLogicalPartitionRelationName),
			queryIncludeRelations2:           cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:                    9,
			expectedTotal:                    9,
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[0].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[0].DeviceInstance),
			expectedInstance:                 inst1,
			wantErr:                          false,
		},
		{
			name: "test NVLinkInterface getall by NVLinkInterfaceStatusProvisioning status success",
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
			queryStatus:                      cutil.GetPtr(cdbm.NVLinkInterfaceStatusProvisioning),
			expectedCount:                    20,
			expectedTotal:                    25,
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvlinklogicalpartitions[0].ID),
			expectedDeviceInstance:           cutil.GetPtr(nvlifcs[0].DeviceInstance),
			wantErr:                          false,
		},
		{
			name: "test NVLinkInterface getall by BadStatus status success",
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
			csh := GetAllNVLinkInterfaceHandler{
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
			if tt.args.reqNvlinkLogicalPartitionID != "" {
				q.Add("nvLinkLogicalPartitionId", tt.args.reqNvlinkLogicalPartitionID)
			}
			if tt.args.reqNVLinkDomainID != nil {
				q.Add("nvLinkDomainId", tt.args.reqNVLinkDomainID.String())
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
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/nvlink-interface", tt.args.reqOrg))
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.reqOrg)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetAllNVLinkInterfaceByInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetAllNVLinkInterfaceByInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := []model.APINVLinkInterface{}
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
				if tt.expectedNVLinkLogicalPartitionID != nil && tt.expectedNVLinkLogicalPartitionID.String() != "" {
					assert.Equal(t, tt.expectedNVLinkLogicalPartitionID.String(), rst[0].NVLinkLogicalPartition.ID)
				}
				if tt.expectedInstance != nil && tt.expectedInstance.ID.String() != "" {
					assert.Equal(t, tt.expectedInstance.ID.String(), rst[0].Instance.ID)
					assert.Equal(t, tt.expectedInstance.Name, rst[0].Instance.Name)
				}
			} else {
				if len(rst) > 0 {
					if tt.expectedInstance != nil && tt.expectedInstance.ID.String() != "" {
						assert.Equal(t, tt.expectedInstance.ID.String(), rst[0].InstanceID)
					}
					if tt.expectedNVLinkLogicalPartitionID != nil && tt.expectedNVLinkLogicalPartitionID.String() != "" {
						assert.Equal(t, tt.expectedNVLinkLogicalPartitionID.String(), rst[0].NVLinkLogicalPartitionID)
					}
					if tt.expectedDeviceInstance != nil {
						assert.Equal(t, *tt.expectedDeviceInstance, rst[0].DeviceInstance)
					}
					if tt.expectedNVLinkDomainID != nil && tt.expectedNVLinkDomainID.String() != "" {
						assert.Equal(t, tt.expectedNVLinkDomainID.String(), *rst[0].NVLinkDomainID)
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

func TestNewGetAllNVLinkInterfaceHandler(t *testing.T) {
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
		want GetAllNVLinkInterfaceHandler
	}{
		{
			name: "test GetAllNVLinkInterfaceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllNVLinkInterfaceHandler{
				dbSession:     dbSession,
				tc:            tc,
				cfg:           cfg,
				queryOverride: nil,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewGetAllNVLinkInterfaceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg)
			if got.dbSession != tt.want.dbSession || got.tc != tt.want.tc || got.cfg != tt.want.cfg || got.queryOverride != nil {
				t.Errorf("NewGetAllNVLinkInterfaceHandler() dbSession=%v tc=%v cfg=%v queryOverride=%v, want queryOverride=nil", got.dbSession, got.tc, got.cfg, got.queryOverride)
			}
		})
	}
}

func TestNewGetAllInstanceNVLinkInterfaceHandler(t *testing.T) {
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
		want GetAllInstanceNVLinkInterfaceHandler
	}{
		{
			name: "test GetAllInstanceNVLinkInterfaceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllInstanceNVLinkInterfaceHandler{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllInstanceNVLinkInterfaceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetAllInstanceNVLinkInterfaceHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestGetAllInstanceNVLinkInterfaceHandler_Handle(t *testing.T) {
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

	nvllps := []*cdbm.NVLinkLogicalPartition{}
	for i := 0; i < 3; i++ {
		nvlinklogicalpartition1 := testBuildNVLinkLogicalPartition(t, dbSession, fmt.Sprintf("test-nvlinklogicalpartition-%d", i), cutil.GetPtr("Test NVLink Logical Partition"), tn1.Org, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
		assert.NotNil(t, nvlinklogicalpartition1)
		nvllps = append(nvllps, nvlinklogicalpartition1)
	}

	nvlifcs := []*cdbm.NVLinkInterface{}
	for i := 0; i < 25; i++ {
		nvllp := nvllps[i%3]
		nvlifc := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st1.ID, inst1.ID, nvllp.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), i%4, cdbm.NVLinkInterfaceStatusProvisioning)
		assert.NotNil(t, nvlifc)
		nvlifcs = append(nvlifcs, nvlifc)
	}

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                             string
		fields                           fields
		args                             args
		wantErr                          bool
		queryStatus                      *string
		queryIncludeRelations1           *string
		queryIncludeRelations2           *string
		pageNumber                       *int
		pageSize                         *int
		orderBy                          *string
		expectedNVLinkLogicalPartitionID *uuid.UUID
		expectedInstanceID               *uuid.UUID
		expectedCount                    int
		expectedTotal                    int
		verifyChildSpanner               bool
		expectedErrorMessage             string
	}{
		{
			name: "test NVLinkInterface getall by Instance API endpoint success",
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
			name: "test NVLinkInterface getall by NVLinkLogicalPartition API endpoint success",
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
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(1),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    10,
			expectedTotal:                    25,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvllps[0].ID),
		},
		{
			name: "test NVLinkInterface getall by NVLinkLogicalPartition success with paging on page 2",
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
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(2),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedCount:                    10,
			expectedTotal:                    25,
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvllps[1].ID),
		},
		{
			name: "test NVLinkInterface getall by NVLinkLogicalPartition error with paging bad orderby",
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
			wantErr:                          false,
			pageNumber:                       cutil.GetPtr(2),
			pageSize:                         cutil.GetPtr(10),
			orderBy:                          cutil.GetPtr("TEST_ASC"),
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvllps[0].ID),
			expectedErrorMessage:             "Failed to validate pagination request data",
		},
		{
			name: "test NVLinkInterface getall by Instance API failure, org does not have a Tenant associated",
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
			name: "test NVLinkInterface getall by Instance API failure, invalid Instance ID in request",
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
			name: "test NVLinkInterface getall by Instance API failure, Instance ID in request not found",
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
			expectedErrorMessage: "Could not find Instance with ID:",
		},
		{
			name: "test NVLinkInterface getall by Instance API failure, Instance not belong to current tenant",
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
			name: "test NVLinkInterface getall by Instance API endpoint success include relation",
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
			queryIncludeRelations1: cutil.GetPtr(cdbm.NVLinkLogicalPartitionRelationName),
			queryIncludeRelations2: cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:          20,
			expectedTotal:          25,
			orderBy:                cutil.GetPtr("CREATED_ASC"),
			expectedInstanceID:     cutil.GetPtr(inst1.ID),
			wantErr:                false,
		},
		{
			name: "test NVLinkInterface getall by NVLinkLogicalPartition InterfaceStatusProvisioning status success",
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
			queryStatus:                      cutil.GetPtr(cdbm.InterfaceStatusProvisioning),
			expectedCount:                    20,
			expectedTotal:                    25,
			orderBy:                          cutil.GetPtr("CREATED_ASC"),
			expectedNVLinkLogicalPartitionID: cutil.GetPtr(nvllps[0].ID),
			wantErr:                          false,
		},
		{
			name: "test NVLinkInterface getall by NVLinkLogicalPartition BadStatus status success",
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
			csh := GetAllInstanceNVLinkInterfaceHandler{
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
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v/nvlinkinterface", tt.args.reqOrg, tt.args.reqInstanceID))
			ec.SetParamNames("orgName", "instanceId")
			if tt.args.reqInstanceID != "" {
				ec.SetParamValues(tt.args.reqOrg, tt.args.reqInstanceID)
			}
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetAllInstanceNVLinkInterfaceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetAllInstanceNVLinkInterfaceHandler.Handle() resp = %v", rec.Body.String())
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

			rst := []model.APINVLinkInterface{}
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
				if tt.expectedNVLinkLogicalPartitionID != nil {
					assert.Equal(t, tt.expectedNVLinkLogicalPartitionID.String(), rst[0].NVLinkLogicalPartitionID)
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
