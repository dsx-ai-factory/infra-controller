// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	"go.temporal.io/api/enums/v1"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestCreateMachineInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	orgRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, orgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)
	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	it := common.TestBuildInstanceType(t, dbSession, "x2.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x2.large",
		"description": "X2 Large Instance Type",
	}, ipu)
	it2 := common.TestBuildInstanceType(t, dbSession, "x2.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x2.large",
		"description": "X2 Large Instance Type",
	}, ipu)
	it3 := common.TestBuildInstanceType(t, dbSession, "x1.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x1.large",
		"description": "X1 Large Instance Type",
	}, ipu)
	icap1 := common.TestCommonBuildMachineCapability(t, dbSession, nil, &it3.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0Hz"), cutil.GetPtr("32GB"), nil, cutil.GetPtr(4), nil, nil)
	assert.NotNil(t, icap1)

	m1 := common.TestBuildMachine(t, dbSession, ip, st, nil, nil, cdbm.MachineStatusReady)
	m2 := common.TestBuildMachine(t, dbSession, ip, st, nil, nil, cdbm.MachineStatusReset)
	m3 := common.TestBuildMachine(t, dbSession, ip, st, nil, nil, cdbm.MachineStatusReset)
	m4 := common.TestBuildMachine(t, dbSession, ip, st, nil, nil, cdbm.MachineStatusError)
	m5 := common.TestBuildMachine(t, dbSession, ip, st, nil, nil, cdbm.MachineStatusReady)

	m6 := common.TestBuildMachine(t, dbSession, ip, st, nil, nil, cdbm.MachineStatusReady)
	mcap1 := common.TestCommonBuildMachineCapability(t, dbSession, &m6.ID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), nil, nil)
	assert.NotNil(t, mcap1)

	mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)
	_, err := mitDAO.Create(ctx, nil, cdbm.MachineInstanceTypeCreateInput{
		MachineID:      m5.ID,
		InstanceTypeID: it2.ID,
	})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()

	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AssociateMachinesWithInstanceType", mock.Anything).Return(wrun, nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AssociateMachinesWithInstanceType", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	assert.Nil(t, err)

	mDAO := cdbm.NewMachineDAO(dbSession)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqData            *model.APIMachineInstanceTypeCreateRequest
		reqJSON            *string
		tc                 temporalClient.Client
		scp                *sc.ClientPool
		reqInstaceTypeID   uuid.UUID
		wantStatusCode     int
		verifyChildSpanner bool
	}{
		{
			name:             "error when request data is an array",
			tc:               tc,
			scp:              scp,
			reqJSON:          cutil.GetPtr("[]"),
			wantStatusCode:   http.StatusBadRequest,
			reqInstaceTypeID: it.ID,
		},
		{
			name: "error when request data does not contain Machine IDs",
			tc:   tc,
			scp:  scp,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{},
			},
			wantStatusCode:   http.StatusBadRequest,
			reqInstaceTypeID: it.ID,
		},
		{
			name: "valid data but nico timeout - fail",
			tc:   tscWithTimeout,
			scp:  scpWithTimeout,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{m1.ID, m2.ID},
			},
			wantStatusCode:     http.StatusInternalServerError,
			verifyChildSpanner: true,
			reqInstaceTypeID:   it.ID,
		},
		{
			name: "test create Machine Instance Type API endpoint",
			tc:   tc,
			scp:  scp,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{m1.ID, m2.ID},
			},
			wantStatusCode:     http.StatusCreated,
			verifyChildSpanner: true,
			reqInstaceTypeID:   it.ID,
		},
		{
			name: "error when request data contain one of the Machine ID is in error state",
			tc:   tc,
			scp:  scp,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{m3.ID, m4.ID},
			},
			wantStatusCode:   http.StatusBadRequest,
			reqInstaceTypeID: it.ID,
		},
		{
			name: "error when machine instance type record already exists for machine",
			tc:   tc,
			scp:  scp,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{m5.ID},
			},
			wantStatusCode:   http.StatusConflict,
			reqInstaceTypeID: it.ID,
		},
		{
			name: "error when request data contain Machine ID is not matching with Instance Type capbilities",
			tc:   tc,
			scp:  scp,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{m6.ID},
			},
			wantStatusCode:   http.StatusConflict,
			reqInstaceTypeID: it3.ID,
		},
		{
			name: "error when request data contain Machine ID doesn't exists",
			tc:   tc,
			scp:  scp,
			reqData: &model.APIMachineInstanceTypeCreateRequest{
				MachineIDs: []string{uuid.New().String()},
			},
			wantStatusCode:   http.StatusConflict,
			reqInstaceTypeID: it3.ID,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cmith := CreateMachineInstanceTypeHandler{
				dbSession: dbSession,
				tc:        tt.tc,
				scp:       tt.scp,
				cfg:       cfg,
			}

			var mitcrJSON []byte
			if tt.reqJSON != nil {
				mitcrJSON = []byte(*tt.reqJSON)
			} else {
				mitcrJSON, _ = json.Marshal(tt.reqData)
			}

			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(mitcrJSON)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "instanceTypeId")
			ec.SetParamValues(ip.Org, tt.reqInstaceTypeID.String())
			ec.Set("user", ipu)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := cmith.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.wantStatusCode, rec.Code)
			if rec.Code != http.StatusCreated {
				fmt.Println(string(rec.Body.Bytes()))
				return
			}

			rst := []model.APIMachineInstanceType{}

			err = json.Unmarshal(rec.Body.Bytes(), &rst)
			require.NoError(t, err)

			assert.Equal(t, len(rst), len(tt.reqData.MachineIDs))

			assert.Equal(t, rst[0].MachineID, m1.ID)
			assert.Equal(t, rst[0].InstanceTypeID, tt.reqInstaceTypeID.String())

			um1, err := mDAO.GetByID(context.Background(), nil, m1.ID, nil, false)
			require.Nil(t, err)
			require.NotNil(t, um1.InstanceTypeID)
			assert.Equal(t, *um1.InstanceTypeID, tt.reqInstaceTypeID)

			assert.Equal(t, rst[1].MachineID, m2.ID)
			assert.Equal(t, rst[1].InstanceTypeID, tt.reqInstaceTypeID.String())

			um2, err := mDAO.GetByID(context.Background(), nil, m2.ID, nil, false)
			require.Nil(t, err)
			require.NotNil(t, um2.InstanceTypeID)
			assert.Equal(t, *um2.InstanceTypeID, tt.reqInstaceTypeID)

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetAllMachineInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	type args struct {
		org   string
		query url.Values
		user  *cdbm.User
	}

	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := common.TestBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnRoles)
	common.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	it := common.TestBuildInstanceType(t, dbSession, "x2.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x2.large",
		"description": "X2 Large Instance Type",
	}, ipu)

	totalCount := 30

	mits := []cdbm.MachineInstanceType{}

	for i := 0; i < totalCount; i++ {
		m := common.TestBuildMachine(t, dbSession, ip, st, &it.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

		mi := common.TestBuildMachineInstanceType(t, dbSession, m, it)

		mits = append(mits, *mi)
	}

	// Setup echo server/context
	e := echo.New()

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		wantFirstEntry     *cdbm.MachineInstanceType
		wantRespCode       int
		verifyChildSpanner bool
	}{
		{
			name: "get all Machine Instance Types  by provider success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:   ipOrg,
				query: url.Values{},
				user:  ipu,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Machine Instance Types by provider with pagination success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"pageNumber": []string{"2"},
					"pageSize":   []string{"5"},
					"orderBy":    []string{"CREATED_DESC"},
				},
				user: ipu,
			},
			wantCount:      5,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantFirstEntry: &mits[24],
		},
		{
			name: "get all Machine Instance Types by provider with pagination failure, invalid page number and order by",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"pageNumber": []string{"2"},
					"pageSize":   []string{"120"},
					"orderBy":    []string{"TEST_ASC"},
				},
				user: ipu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
		},
		{
			name: "get all Machine Instance Types by Tenant failure, org must have Provider",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg,
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gamith := GetAllMachineInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/instance/type/%s/machine?%s", tt.args.org, it.ID.String(), tt.args.query.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "instanceTypeId")
			ec.SetParamValues(tt.args.org, it.ID.String())
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gamith.Handle(ec)
			assert.NoError(t, err)

			if tt.wantRespCode != rec.Code {
				t.Errorf("response = %v", rec.Body.String())
			}

			require.Equal(t, tt.wantRespCode, rec.Code)

			if tt.wantRespCode != http.StatusOK {
				return
			}

			rst := []model.APIMachineInstanceType{}

			err = json.Unmarshal(rec.Body.Bytes(), &rst)
			require.NoError(t, err)

			assert.Equal(t, tt.wantCount, len(rst))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			require.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			require.NoError(t, err)

			assert.Equal(t, tt.wantTotalCount, pr.Total)

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.MachineID, rst[0].MachineID)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDeleteMachineInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	orgRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, orgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)
	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	it := common.TestBuildInstanceType(t, dbSession, "x2.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x2.large",
		"description": "X2 Large Instance Type",
	}, ipu)
	it2 := common.TestBuildInstanceType(t, dbSession, "x3.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x3.large",
		"description": "X3 Large Instance Type",
	}, ipu)
	it3 := common.TestBuildInstanceType(t, dbSession, "x4.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x4.large",
		"description": "X4 Large Instance Type",
	}, ipu)
	it4 := common.TestBuildInstanceType(t, dbSession, "x5.large", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "x5.large",
		"description": "X5 Large Instance Type",
	}, ipu)

	m := common.TestBuildMachine(t, dbSession, ip, st, &it.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	m2 := common.TestBuildMachine(t, dbSession, ip, st, &it2.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	m3 := common.TestBuildMachine(t, dbSession, ip, st, &it2.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

	m4 := common.TestBuildMachine(t, dbSession, ip, st, &it4.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	m5 := common.TestBuildMachine(t, dbSession, ip, st, &it4.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	m6 := common.TestBuildMachine(t, dbSession, ip, st, &it4.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	m7 := common.TestBuildMachine(t, dbSession, ip, st, &it4.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	m8 := common.TestBuildMachine(t, dbSession, ip, st, &it.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

	assert.Equal(t, it.ID, *m.InstanceTypeID)
	assert.Equal(t, it2.ID, *m2.InstanceTypeID)
	assert.Equal(t, it2.ID, *m3.InstanceTypeID)

	mit := common.TestBuildMachineInstanceType(t, dbSession, m, it)
	mit2 := common.TestBuildMachineInstanceType(t, dbSession, m2, it2)
	mit3 := common.TestBuildMachineInstanceType(t, dbSession, m3, it3)

	mit4 := common.TestBuildMachineInstanceType(t, dbSession, m4, it4)
	_ = common.TestBuildMachineInstanceType(t, dbSession, m5, it4)
	_ = common.TestBuildMachineInstanceType(t, dbSession, m6, it4)

	mit7 := common.TestBuildMachineInstanceType(t, dbSession, m7, it)
	mit8 := common.TestBuildMachineInstanceType(t, dbSession, m8, it)

	// create an allocation for the instance type it2
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := common.TestBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnRoles)
	tenant := common.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	// Create Allocation for Instance Type 2
	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it2.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 1}
	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit1", Description: cutil.GetPtr(""), TenantID: tenant.ID.String(), SiteID: st.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	testCreateAllocation(t, dbSession, ipamStorage, ipu, org, string(okBodyIT))

	// Create Allocation for Instance Type 3
	acGoodIT2 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it3.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 1}
	okBodyIT2, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit2", Description: cutil.GetPtr(""), TenantID: tenant.ID.String(), SiteID: st.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT2}})
	assert.Nil(t, err)
	apial := testCreateAllocation(t, dbSession, ipamStorage, ipu, org, string(okBodyIT2))

	// Create Allocation for Instance Type 4
	acGoodIT4 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it4.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 3}
	okBodyIT4, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit4", Description: cutil.GetPtr(""), TenantID: tenant.ID.String(), SiteID: st.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT4}})
	assert.Nil(t, err)
	apial2 := testCreateAllocation(t, dbSession, ipamStorage, ipu, org, string(okBodyIT4))
	assert.NotNil(t, apial2)

	vpc := common.TestBuildVPC(t, dbSession, "test-vpc", ip, tenant, st, cutil.GetPtr(uuid.New()), nil, nil, cdbm.VpcStatusReady, tnu)
	os := common.TestBuildOperatingSystem(t, dbSession, "test-os", tenant, cdbm.OperatingSystemStatusReady, tnu)

	alDAO := cdbm.NewAllocationDAO(dbSession)
	alcDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	al, err := alDAO.GetByID(context.Background(), nil, uuid.MustParse(apial.ID), nil)
	assert.NoError(t, err)
	alcs, _, err := alcDAO.GetAll(context.Background(), nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{al.ID}}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 1, len(alcs))

	// Create an Instance with Machine 3
	common.TestBuildInstance(t, dbSession, "test-instance", tenant.ID, ip.ID, st.ID, it3.ID, vpc.ID, &m3.ID, os.ID)

	cfg := common.GetTestConfig()

	mDAO := cdbm.NewMachineDAO(dbSession)

	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RemoveMachineInstanceTypeAssociation", mock.Anything).Return(wrun, nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[st.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RemoveMachineInstanceTypeAssociation", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RemoveMachineInstanceTypeAssociation", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
		scp       *sc.ClientPool
	}
	type args struct {
		it                     *cdbm.InstanceType
		mit                    *cdbm.MachineInstanceType
		deleteID               string
		expectedDeletedMachine string
		org                    string
		user                   *cdbm.User
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantStatusCode     int
		verifyChildSpanner bool
	}{
		{
			name: "test delete Machine Instance Type API endpoint with timeout - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithTimeout,
				scp:       scpWithTimeout,
				cfg:       cfg,
			},
			args: args{
				it:                     it,
				mit:                    mit,
				deleteID:               mit.ID.String(),
				expectedDeletedMachine: m.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode:     http.StatusInternalServerError,
			verifyChildSpanner: true,
		},
		{
			name: "test delete Machine Instance Type API endpoint",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				it:                     it,
				mit:                    mit,
				deleteID:               mit.ID.String(),
				expectedDeletedMachine: m.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name: "test delete Machine Instance Type API endpoint by machine ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				it:                     it,
				mit:                    mit8,
				deleteID:               m8.ID,
				expectedDeletedMachine: m8.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name: "test delete Machine Instance Type API endpoint again - nico not found - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoNotFound,
				scp:       scpWithNICoNotFound,
				cfg:       cfg,
			},
			args: args{
				it:                     it,
				mit:                    mit7,
				deleteID:               mit7.ID.String(),
				expectedDeletedMachine: m7.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name: "error when allocation constraints for instance type is violated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				it:                     it2,
				mit:                    mit2,
				deleteID:               mit2.ID.String(),
				expectedDeletedMachine: m2.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode: http.StatusBadRequest,
		},
		{
			name: "error when Machine is in use by Instance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				it:                     it3,
				mit:                    mit3,
				deleteID:               mit3.ID.String(),
				expectedDeletedMachine: m3.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode: http.StatusBadRequest,
		},
		{
			name: "error when allocation constraints for instance type is violated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				it:                     it4,
				mit:                    mit4,
				deleteID:               mit4.ID.String(),
				expectedDeletedMachine: m4.ID,
				org:                    org,
				user:                   ipu,
			},
			wantStatusCode: http.StatusBadRequest,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dmith := DeleteMachineInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			e := echo.New()

			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "instanceTypeId", "id")
			ec.SetParamValues(tt.args.org, tt.args.it.ID.String(), tt.args.deleteID)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := dmith.Handle(ec)
			require.NoError(t, err)

			fmt.Printf("response: %v", rec.Body.String())
			assert.Equal(t, tt.wantStatusCode, rec.Code)

			if tt.wantStatusCode != http.StatusAccepted {
				return
			}

			mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)
			umits, _, terr := mitDAO.GetAll(context.Background(), nil, cdbm.MachineInstanceTypeFilterInput{
				MachineID:       cutil.GetPtr(tt.args.expectedDeletedMachine),
				InstanceTypeIDs: []uuid.UUID{tt.args.it.ID},
			}, paginator.PageInput{}, nil)
			assert.Nil(t, terr)
			assert.Len(t, umits, 0)

			// Verify the requested machine no longer carries the Instance Type assignment.
			updatedMachine, err := mDAO.GetByID(context.Background(), nil, tt.args.expectedDeletedMachine, nil, false)
			require.NoError(t, err)
			assert.Nil(t, updatedMachine.InstanceTypeID)

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineInstanceTypeHandlers(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	cmith := CreateMachineInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}

	if got := NewCreateMachineInstanceTypeHandler(dbSession, tc, scp, cfg); !reflect.DeepEqual(got, cmith) {
		t.Errorf("CreateMachineInstanceTypeHandler() = %v, want %v", got, cmith)
	}

	gamith := GetAllMachineInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}

	if got := NewGetAllMachineInstanceTypeHandler(dbSession, tc, cfg); !reflect.DeepEqual(got, gamith) {
		t.Errorf("GetAllMachineInstanceTypeHandler() = %v, want %v", got, gamith)
	}

	dmith := DeleteMachineInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}

	if got := NewDeleteMachineInstanceTypeHandler(dbSession, tc, scp, cfg); !reflect.DeepEqual(got, dmith) {
		t.Errorf("DeleteMachineInstanceTypeHandler() = %v, want %v", got, dmith)
	}
}
