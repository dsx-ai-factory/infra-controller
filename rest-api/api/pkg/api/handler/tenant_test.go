// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
)

func testTenantInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testTenantSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// Create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// Create user table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create TenantAccount table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
}

func TestCreateTenantHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := testTenantInitDB(t)
	defer dbSession.Close()

	testTenantSetupSchema(t, dbSession)

	tnOrg := "test-tenant-org-1"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		cfg       *config.Config
	}
	type args struct {
		org  string
		user *cdbm.User
		ta   *cdbm.TenantAccount
	}
	tests := []struct {
		name           string
		fields         fields
		args           args
		wantStatusCode int
	}{
		{
			name: "test create Tenant failure, endpoint deprecated",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       common.GetTestConfig(),
			},
			args: args{
				org:  tnOrg,
				user: tnu1,
			},
			wantStatusCode: http.StatusBadRequest,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctnh := CreateTenantHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			err := ctnh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.wantStatusCode, rec.Code)
		})
	}
}

func TestGetCurrentTenantHandler_Handle(t *testing.T) {
	// Set up DB
	ctx := context.Background()
	dbSession := testTenantInitDB(t)
	defer dbSession.Close()

	testTenantSetupSchema(t, dbSession)

	// Add user entry
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)

	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnRoles := []string{authz.TenantAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnu1 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	tn1 := common.TestBuildTenant(t, dbSession, "Test Tenant 1", tnOrg1, tnu1)
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn1.ID, tnOrg1, cdbm.TenantAccountStatusReady, tnu1)

	tnOrg2 := "test-tenant-org-2"
	tnu2 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)

	ta2 := common.TestBuildTenantAccount(t, dbSession, ip, nil, tnOrg2, cdbm.TenantAccountStatusPending, tnu2)

	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		org  string
		user *cdbm.User
		ta   *cdbm.TenantAccount
	}
	tests := []struct {
		name                         string
		fields                       fields
		args                         args
		wantTenant                   *cdbm.Tenant
		wantTargetedInstanceCreation *bool
		wantTenantAccountSync        bool
		wantStatusCode               int
		verifyChildSpanner           bool
	}{
		{
			name: "test get current Tenant success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg1,
				user: tnu1,
			},
			wantTenant:                   tn1,
			wantTargetedInstanceCreation: cutil.GetPtr(true),
			wantStatusCode:               http.StatusOK,
		},
		{
			name: "test get current Tenant success, auto-created when it does not exist",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg2,
				user: tnu2,
				ta:   ta2,
			},
			wantTenantAccountSync: true,
			wantStatusCode:        http.StatusOK,
			verifyChildSpanner:    true,
		},
		{
			name: "get current Infrastructure Provider failure, user doesn't have the right role",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipu,
			},
			wantStatusCode: http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			gctnh := GetCurrentTenantHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}
			err := gctnh.Handle(ec)
			assert.NoError(t, err)

			require.Equal(t, rec.Code, tt.wantStatusCode)
			if tt.wantStatusCode != http.StatusOK {
				return
			}

			rtn := &model.APITenant{}

			err = json.Unmarshal(rec.Body.Bytes(), rtn)
			assert.NoError(t, err)

			assert.NotEmpty(t, rtn.ID)
			require.NotNil(t, rtn.Capabilities)
			assert.Equal(t, tt.wantTargetedInstanceCreation, rtn.Capabilities.TargetedInstanceCreation)
			if tt.wantTenant != nil {
				assert.Equal(t, tt.wantTenant.ID.String(), rtn.ID)
				assert.Equal(t, tt.wantTenant.Org, rtn.Org)
				assert.Equal(t, tt.wantTenant.Org, *rtn.OrgDisplayName)
			}

			if tt.wantTenantAccountSync {
				taDAO := cdbm.NewTenantAccountDAO(tt.fields.dbSession)
				ta, serr := taDAO.GetByID(context.Background(), nil, tt.args.ta.ID, nil)
				require.NoError(t, serr)
				require.NotNil(t, ta.TenantID)
				assert.Equal(t, ta.TenantID.String(), rtn.ID)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetCurrentTenantStatsHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipu := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnOrgRoles)
	tn1 := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnOrgRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "test-tenant-1", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	// Create VPC
	st := testVPCBuildSite(t, dbSession, ip, "test-site-1", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn1, "test-allocation", ipu)
	assert.NotNil(t, al)

	vpc := testVPCBuildVPC(t, dbSession, "test-vpc", ip, tn1, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "east1"}, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc)

	domain := testSubnetBuildDomain(t, dbSession, "test.com", tnOrg1, &tnu1.ID)
	assert.NotNil(t, domain)

	// Create TenantAccount
	ta11 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip, tn1, tn1.Org, cdbm.TenantAccountStatusInvited, ipu.ID, uuid.Nil)
	assert.NotNil(t, ta11)
	testTenantAccountBuildStatusDetail(t, dbSession, ta11.ID, ta11.Status)
	testTenantAccountBuildStatusDetail(t, dbSession, ta11.ID, cdbm.TenantAccountStatusReady)

	// Create Subnet
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", st, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(context.Background(), ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	prefixLen := 24
	parentIpbBody, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	subnet := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu1, tnOrg1, string(parentIpbBody))
	assert.NotNil(t, subnet)

	// Create Instance
	al1 := testInstanceSiteBuildAllocation(t, dbSession, st, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	instsub1 := testInstanceBuildInstanceInterface(t, dbSession, inst1.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusPending)
	assert.NotNil(t, instsub1)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		c echo.Context
	}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		reqCurrTenant      *cdbm.Tenant
		reqCurrentUser     *cdbm.User
		reqEmpty           bool
		reqVpcCount        int
		reqSubnetCount     int
		reqInstanceCount   int
		reqTaCount         int
		verifyChildSpanner bool
	}{
		{
			name: "test Tenant get current stats API endpoint return stats",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       common.GetTestConfig(),
			},
			wantErr:            false,
			reqEmpty:           false,
			reqVpcCount:        3,
			reqSubnetCount:     3,
			reqInstanceCount:   1,
			reqTaCount:         1,
			reqCurrTenant:      tn1,
			reqCurrentUser:     tnu1,
			verifyChildSpanner: true,
		},
		{
			name: "test Tenant get current stats API endpoint return no stats",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       common.GetTestConfig(),
			},
			wantErr:          false,
			reqEmpty:         true,
			reqVpcCount:      0,
			reqSubnetCount:   0,
			reqInstanceCount: 0,
			reqTaCount:       0,
			reqCurrTenant:    tn2,
			reqCurrentUser:   tnu2,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqCurrTenant.Org)
			ec.Set("user", tt.reqCurrentUser)

			ec.SetRequest(ec.Request().WithContext(ctx))

			gctnsh := GetCurrentTenantStatsHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}
			if err := gctnsh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetCurrentTenantStatsHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			assert.Equal(t, http.StatusOK, rec.Code)

			fmt.Printf("%v", rec.Body.String())

			rst := &model.APITenantStats{}
			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, rst.Vpc.Total, tt.reqVpcCount)
			assert.Equal(t, rst.Subnet.Total, tt.reqSubnetCount)
			assert.Equal(t, rst.Instance.Total, tt.reqInstanceCount)
			assert.Equal(t, rst.TenantAccount.Total, tt.reqTaCount)

			if !tt.reqEmpty {
				// VPC assert
				assert.Equal(t, rst.Vpc.Pending, 1)
				assert.Equal(t, rst.Vpc.Ready, 2)

				// Subnet assert
				assert.Equal(t, rst.Subnet.Pending, 2)
				assert.Equal(t, rst.Subnet.Ready, 1)

				// Instance assert
				assert.Equal(t, rst.Instance.Ready, 1)

				// TenantAccount assert
				assert.Equal(t, rst.TenantAccount.Invited, 1)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

		})
	}
}

func TestUpdateTenantHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := testTenantInitDB(t)
	defer dbSession.Close()

	testTenantSetupSchema(t, dbSession)

	// Add user entry
	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testSiteBuildUser(t, dbSession, "test456", tnOrg, tnRoles)

	cfg := common.GetTestConfig()

	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		cfg       *config.Config
	}

	type args struct {
		org  string
		user *cdbm.User
	}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		fields         fields
		args           args
		wantStatusCode int
	}{
		{
			name: "test Tenant update failure, endpoint deprecated",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg,
				user: tnu,
			},
			wantStatusCode: http.StatusBadRequest,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ucth := UpdateCurrentTenantHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.Set("user", tt.args.user)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)

			ec.SetRequest(ec.Request().WithContext(ctx))

			err := ucth.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.wantStatusCode, rec.Code)
		})
	}
}
