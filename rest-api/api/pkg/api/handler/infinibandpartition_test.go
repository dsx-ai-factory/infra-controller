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
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
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
)

// We have a lot of per-object-test duplicates of functions
// like this.  We should be able to reduce them all down to some shared common
// ones like this and move this into the common lib.
func testBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
	alDAO := cdbm.NewAllocationDAO(dbSession)

	createInput := cdbm.AllocationCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("Test Allocation Description"),
		InfrastructureProviderID: st.InfrastructureProviderID,
		TenantID:                 tn.ID,
		SiteID:                   st.ID,
		Status:                   cdbm.AllocationStatusPending,
		CreatedBy:                user.ID,
	}
	al, err := alDAO.Create(context.Background(), nil, createInput)
	assert.Nil(t, err)

	return al
}

func testBuildIBPartition(t *testing.T, dbSession *cdb.Session, name string, org string, site *cdbm.Site, tenant *cdbm.Tenant, controllerIBPartionID *uuid.UUID, status *cdbm.InfiniBandPartitionStatus, isMissingOnSite bool) *cdbm.InfiniBandPartition {
	if status == nil {
		status = cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady)
	}
	ibp := &cdbm.InfiniBandPartition{
		ID:                      uuid.New(),
		Name:                    name,
		Description:             cutil.GetPtr("Test InfiniBand Partition"),
		Org:                     org,
		SiteID:                  site.ID,
		TenantID:                tenant.ID,
		ControllerIBPartitionID: controllerIBPartionID,
		Status:                  *status,
		IsMissingOnSite:         isMissingOnSite,
	}
	_, err := dbSession.DB.NewInsert().Model(ibp).Exec(context.Background())
	assert.Nil(t, err)
	return ibp
}

func TestInfiniBandPartitionHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	// Add user entry
	ipOrg1 := "test-ip-org-1"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles1)
	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles2)
	tnu3 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg3}, tnRoles1)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	// This site should get no allocations to verify that
	// creation and updates fail when a tenant has no allocations
	// at a site.
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", nil, nil)
	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	site3 := testFabricBuildSite(t, dbSession, ip1, "testSite3", nil, nil)
	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site3.ID, tnu1.ID)
	assert.NotNil(t, ts3)

	site4 := testFabricBuildSite(t, dbSession, ip1, "testSite4", nil, nil)
	ts4 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site4.ID, tnu1.ID)
	assert.NotNil(t, ts4)

	al := testBuildAllocation(t, dbSession, site1, tn1, "test-allocation", tnu1)
	assert.NotNil(t, al)

	al2 := testBuildAllocation(t, dbSession, site3, tn1, "test-allocation-2", tnu1)
	assert.NotNil(t, al2)

	al3 := testBuildAllocation(t, dbSession, site4, tn1, "test-allocation-3", tnu1)
	assert.NotNil(t, al3)

	ibpObj := model.APIInfiniBandPartitionCreateRequest{Name: "test-ibp-1", Description: cutil.GetPtr("test"), SiteID: site1.ID.String(), Labels: map[string]string{"test-label-1": "test-value-1"}}
	okBody, err := json.Marshal(ibpObj)
	assert.Nil(t, err)

	ibpObj1 := model.APIInfiniBandPartitionCreateRequest{Name: "test-ibp-2", Description: cutil.GetPtr("test")}
	errBodyMissingSite, err := json.Marshal(ibpObj1)
	assert.Nil(t, err)

	ibpObj4 := model.APIInfiniBandPartitionCreateRequest{Name: "test-ibp-5", Description: cutil.GetPtr("test"), SiteID: "124"}
	errBodyInvalidSite, err := json.Marshal(ibpObj4)
	assert.Nil(t, err)

	ibpObj5 := model.APIInfiniBandPartitionCreateRequest{Name: "test-ibp-no-allocations", Description: cutil.GetPtr("test"), SiteID: site2.ID.String()}
	errNoAllocations, err := json.Marshal(ibpObj5)
	assert.Nil(t, err)

	ibpObj6 := model.APIInfiniBandPartitionCreateRequest{Name: "test-ibp-6", Description: cutil.GetPtr("test"), SiteID: site3.ID.String()}
	okBody2, err := json.Marshal(ibpObj6)
	assert.Nil(t, err)

	ibpObj7 := model.APIInfiniBandPartitionCreateRequest{Name: "test-ibp-1", Description: cutil.GetPtr("test"), SiteID: site4.ID.String()}
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(ibpObj)
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)

	noNameClashBody, err := json.Marshal(ibpObj7)
	assert.Nil(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	e := echo.New()
	cfg := common.GetTestConfig()

	// Mock per-Site client for site1
	tsc := &tmocks.Client{}

	// Mock per-Site client for site2
	tsc2 := &tmocks.Client{}

	// Mock per-Site client for site4
	tsc4 := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site1.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc
	scp.IDClientMap[site3.ID.String()] = tsc2
	scp.IDClientMap[site4.ID.String()] = tsc4

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun4 := &tmocks.WorkflowRun{}
	wrun4.On("GetID").Return(wid)

	// Mock create call for CreateInfiniBandPartitionV2 workflow
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInfiniBandPartitionV2", mock.Anything).Return(wrun, nil)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	// Mock create call for CreateInfiniBandPartitionV2 workflow
	tsc4.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInfiniBandPartitionV2", mock.Anything).Return(wrun4, nil)

	wrun4.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	// Mock timeout error for timeout test case
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")
	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	// Mock timeout workflow execution
	tsc2.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInfiniBandPartitionV2", mock.Anything).Return(wruntimeout, nil).Once()

	tsc2.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		fields             fields
		name               string
		reqOrgName         string
		reqBody            string
		reqBodyModel       *model.APIInfiniBandPartitionCreateRequest
		user               *cdbm.User
		expectedStatus     int
		expectedName       bool
		verifyChildSpanner bool
	}{
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody),
			reqBodyModel:   &ibpObj,
			user:           nil,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody),
			reqBodyModel:   &ibpObj,
			user:           tnu1,
			expectedStatus: http.StatusForbidden,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when org tenant is not admin",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody),
			reqBodyModel:   &ibpObj,
			user:           tnu2,
			expectedStatus: http.StatusForbidden,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when request body doesnt bind",
			reqOrgName:     tnOrg1,
			reqBody:        "SomeNonJsonBody",
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when request doesnt validate",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyDoesntValidate),
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			reqBody:        string(okBody),
			user:           tnu3,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when site id not specified in request",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyMissingSite),
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when site id is invalid specified in request",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyInvalidSite),
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:               "error when a tenant has no allocations at a site",
			reqOrgName:         tnOrg1,
			reqBody:            string(errNoAllocations),
			reqBodyModel:       &ibpObj5,
			user:               tnu1,
			expectedStatus:     http.StatusForbidden,
			verifyChildSpanner: true,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:               "success case with CreateInfiniBandPartitionV2 workflow",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody),
			reqBodyModel:       &ibpObj,
			user:               tnu1,
			expectedStatus:     http.StatusCreated,
			verifyChildSpanner: true,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc2,
				cfg:       cfg,
			},
			name:               "timeout case with CreateInfiniBandPartitionV2 workflow",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody2),
			reqBodyModel:       &ibpObj6,
			user:               tnu1,
			expectedStatus:     http.StatusInternalServerError,
			verifyChildSpanner: true,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error due to name clash in same site",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			expectedStatus: http.StatusConflict,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc4,
				cfg:       cfg,
			},
			name:               "partition created successfully with same name with different site",
			reqOrgName:         tnOrg1,
			reqBody:            string(noNameClashBody),
			reqBodyModel:       &ibpObj7,
			user:               tnu1,
			expectedStatus:     http.StatusCreated,
			verifyChildSpanner: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			// Setup echo server/context
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			cibph := CreateInfiniBandPartitionHandler{
				dbSession: tc.fields.dbSession,
				tc:        tc.fields.tc,
				scp:       scp,
				cfg:       tc.fields.cfg,
			}
			err := cibph.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if rec.Code == http.StatusCreated {
				rsp := &model.APIInfiniBandPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 1)
				assert.Equal(t, rsp.Name, tc.reqBodyModel.Name)
				wantLabels := tc.reqBodyModel.Labels
				if wantLabels == nil {
					wantLabels = map[string]string{}
				}
				assert.Equal(t, wantLabels, map[string]string(rsp.Labels))
				assert.Equal(t, rsp.Status, cdbm.InfiniBandPartitionStatusPending)
			}
			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandPartitionHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"
	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles1)
	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles2)
	tnu3 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg3}, tnRoles1)
	tnu4 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg4}, tnRoles1)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	tn2 := testFabricBuildTenant(t, dbSession, tnOrg4, "testTenant2")
	assert.NotNil(t, tn2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn2.ID, site2.ID, tnu4.ID)
	assert.NotNil(t, ts2)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	ibpDAO := cdbm.NewInfiniBandPartitionDAO(dbSession)

	totalCount := 30

	ibps := []cdbm.InfiniBandPartition{}

	for i := 0; i < totalCount; i++ {
		ipb, err := ibpDAO.Create(
			ctx,
			nil,
			cdbm.InfiniBandPartitionCreateInput{
				Name:        fmt.Sprintf("test-ipb-%02d", i),
				Description: cutil.GetPtr("test"),
				TenantOrg:   tnOrg1,
				SiteID:      site1.ID,
				TenantID:    tn1.ID,
				Labels:      map[string]string{"test-ibp-labels-key-%02d": fmt.Sprintf("test-ibp-labels-value-%02d", i)},
				Status:      cdbm.InfiniBandPartitionStatusPending,
				CreatedBy:   tnu1.ID,
			},
		)
		assert.Nil(t, err)
		common.TestBuildStatusDetail(t, dbSession, ipb.ID.String(), string(cdbm.InfiniBandPartitionStatusPending), cutil.GetPtr("request received, pending processing"))
		common.TestBuildStatusDetail(t, dbSession, ipb.ID.String(), string(cdbm.InfiniBandPartitionStatusReady), cutil.GetPtr("InfiniBand Partition is now ready for use"))
		ibps = append(ibps, *ipb)
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                   string
		reqOrgName             string
		user                   *cdbm.User
		querySiteID            *string
		querySearch            *string
		queryStatus            *string
		queryIncludeRelations1 *string
		queryIncludeRelations2 *string
		queryIncludeRelations3 *string
		pageNumber             *int
		pageSize               *int
		orderBy                *string
		expectedErr            bool
		expectedStatus         int
		expectedCnt            int
		expectedTotal          *int
		expectedFirstEntry     *cdbm.InfiniBandPartition
		expectedTenantOrg      *string
		expectedSite           *cdbm.Site
		verifyChildSpanner     bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg2,
			user:           nil,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user is not a member of org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			user:           tnu3,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant is not admin for org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when Site ID specified in query is an invalid UUID",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr("bad#uuid$str"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when non-existent Site ID is specified in query",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(uuid.New().String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case when objects returned",
			reqOrgName:         tnOrg1,
			user:               tnu1,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			verifyChildSpanner: true,
		},
		{
			name:                   "success when tenant relation are specified",
			reqOrgName:             tnOrg1,
			user:                   tnu1,
			querySiteID:            cutil.GetPtr(site1.ID.String()),
			queryIncludeRelations1: cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			expectedTenantOrg:      cutil.GetPtr(tn1.Org),
		},
		{
			name:                   "success when site relation are specified",
			reqOrgName:             tnOrg1,
			user:                   tnu1,
			querySiteID:            cutil.GetPtr(site1.ID.String()),
			queryIncludeRelations1: cutil.GetPtr(cdbm.SiteRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			expectedSite:           site1,
		},
		{
			name:           "success case when no objects returned",
			reqOrgName:     tnOrg4,
			user:           tnu4,
			querySiteID:    cutil.GetPtr(site2.ID.String()),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
		},
		{
			name:               "success when pagination params are specified",
			reqOrgName:         tnOrg1,
			user:               tnu1,
			querySiteID:        cutil.GetPtr(site1.ID.String()),
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(10),
			orderBy:            cutil.GetPtr("NAME_DESC"),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        10,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectedFirstEntry: &ibps[29],
		},
		{
			name:           "failure when invalid pagination params are specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:           "success when name query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			querySearch:    cutil.GetPtr("test"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when unexisted status query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			querySearch:    cutil.GetPtr("ready"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success when name and status query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			querySearch:    cutil.GetPtr("test ready"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when labels query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			querySearch:    cutil.GetPtr("test-ibp-labels-key"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when InfiniBandPartitionStatusPending status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			queryStatus:    cdb.GetTypedStrPtr(cdbm.InfiniBandPartitionStatusPending),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when BadStatus status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			queryStatus:    cutil.GetPtr("BadRequest"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.querySiteID != nil {
				q.Add("siteId", *tc.querySiteID)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.queryStatus != nil {
				q.Add("status", *tc.queryStatus)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations3)
			}
			if tc.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tc.pageNumber))
			}
			if tc.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tc.pageSize))
			}
			if tc.orderBy != nil {
				q.Set("orderBy", *tc.orderBy)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/infiniband-partition?%s", tc.reqOrgName, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			ibpah := GetAllInfiniBandPartitionHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := ibpah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			require.Equal(t, tc.expectedStatus, rec.Code)

			if !tc.expectedErr {
				rsp := []model.APIInfiniBandPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), &rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedCnt, len(rsp))
				for i := 0; i < tc.expectedCnt; i++ {
					assert.Equal(t, tn1.ID.String(), rsp[i].TenantID)
				}

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil {
					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp[0].Tenant.Org)
					}
					if tc.expectedSite != nil {
						assert.Equal(t, tc.expectedSite.Name, rsp[0].Site.Name)
					}
				} else {
					if len(rsp) > 0 {
						assert.Nil(t, rsp[0].Tenant)
						assert.Nil(t, rsp[0].Site)
					}
				}

				for _, apios := range rsp {
					assert.Equal(t, 2, len(apios.StatusHistory))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandPartitionHandler_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"
	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles1)
	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles2)
	tnu3 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg3}, tnRoles1)
	tnu4 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg4}, tnRoles1)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	tn2 := testFabricBuildTenant(t, dbSession, tnOrg4, "testTenant2")
	assert.NotNil(t, tn2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn2.ID, site2.ID, tnu4.ID)
	assert.NotNil(t, ts2)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg1, site1, tn1, nil, nil, false)
	assert.NotNil(t, ibp1)

	ibp2 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg2, site2, tn2, nil, nil, false)
	assert.NotNil(t, ibp2)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                   string
		reqOrgName             string
		user                   *cdbm.User
		ibpID                  string
		queryIncludeRelations1 *string
		queryIncludeRelations2 *string
		queryIncludeRelations3 *string
		expectedErr            bool
		expectedStatus         int
		expectedTenantOrg      *string
		expectedSite           *cdbm.Site
		expectIBP              *cdbm.InfiniBandPartition
		verifyChildSpanner     bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg2,
			user:           nil,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user is not a member of org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			user:           tnu3,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant is not admin for org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when ipb id doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			ibpID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when ipb tenant doesnt match tenant in org",
			reqOrgName:     tnOrg4,
			user:           tnu4,
			ibpID:          ibp1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case 1",
			reqOrgName:         tnOrg1,
			user:               tnu1,
			ibpID:              ibp1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectIBP:          ibp1,
			verifyChildSpanner: true,
		},
		{
			name:               "success case 2",
			reqOrgName:         tnOrg4,
			user:               tnu4,
			ibpID:              ibp2.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectIBP:          ibp2,
			verifyChildSpanner: true,
		},
		{
			name:                   "success when both site and fabric relations are specified",
			reqOrgName:             tnOrg1,
			user:                   tnu1,
			ibpID:                  ibp1.ID.String(),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeRelations2: cutil.GetPtr(cdbm.SiteRelationName),
			expectedSite:           site1,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)

			q := req.URL.Query()
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}

			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}

			if tc.queryIncludeRelations3 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations3)
			}

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.ibpID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			ibpgh := GetInfiniBandPartitionHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := ibpgh.Handle(ec)
			assert.Nil(t, err)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			assert.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if !tc.expectedErr {
				rsp := &model.APIInfiniBandPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				if tc.expectIBP != nil {
					assert.Equal(t, tc.expectIBP.Name, rsp.Name)
					assert.Equal(t, tc.expectIBP.TenantID.String(), rsp.TenantID)
				}

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil {
					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
					if tc.expectedSite != nil {
						assert.Equal(t, tc.expectedSite.Name, rsp.Site.Name)
					}
				} else {
					assert.Nil(t, rsp.Tenant)
					assert.Nil(t, rsp.Site)
				}

			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandPartitionHandle_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"
	tnOrg5 := "test-tn-org-5"

	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles1)
	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles2)
	tnu3 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg3}, tnRoles1)
	tnu4 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg4}, tnRoles1)
	tnu5 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg4}, tnRoles1)
	tnu6 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg5}, tnRoles1)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", nil, nil)

	// This site should get no allocations to verify that
	// creation and updates fail when a tenant has no allocations
	// at a site.
	site3 := testFabricBuildSite(t, dbSession, ip1, "testSite2", nil, nil)
	site4 := testFabricBuildSite(t, dbSession, ip1, "testSite4", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	tn2 := testFabricBuildTenant(t, dbSession, tnOrg4, "testTenant2")
	assert.NotNil(t, tn2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn2.ID, site2.ID, tnu4.ID)
	assert.NotNil(t, ts2)

	tn3 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant3")
	assert.NotNil(t, tn3)

	tn4 := testFabricBuildTenant(t, dbSession, tnOrg5, "testTenant4")
	assert.NotNil(t, tn4)

	// For testing a site with no allocations for tenant
	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn3.ID, site3.ID, tnu5.ID)
	assert.NotNil(t, ts3)

	ts4 := testBuildTenantSiteAssociation(t, dbSession, tnOrg5, tn4.ID, site4.ID, tnu6.ID)
	assert.NotNil(t, ts4)

	al := testBuildAllocation(t, dbSession, site1, tn1, "test-allocation", tnu1)
	assert.NotNil(t, al)

	al2 := testBuildAllocation(t, dbSession, site2, tn2, "test-allocation", tnu4)
	assert.NotNil(t, al2)

	al3 := testBuildAllocation(t, dbSession, site4, tn4, "test-allocation", tnu6)
	assert.NotNil(t, al3)

	cfg := common.GetTestConfig()

	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg1, site1, tn1, nil, nil, false)
	assert.NotNil(t, ibp1)

	ibp2 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg2, site2, tn2, nil, nil, false)
	assert.NotNil(t, ibp2)

	ibp3 := testBuildIBPartition(t, dbSession, "test-ibp-3", tnOrg1, site1, tn1, nil, nil, false)
	assert.NotNil(t, ibp3)

	// For testing a site with no allocations for tenant
	ibp4 := testBuildIBPartition(t, dbSession, "test-ibp-3", tnOrg1, site3, tn3, nil, nil, false)
	assert.NotNil(t, ibp4)

	ibp5 := testBuildIBPartition(t, dbSession, "test-ibp-5", tnOrg5, site4, tn4, nil, nil, false)
	assert.NotNil(t, ibp5)

	// Populate request data
	errBody1, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("a")})
	assert.NotNil(t, errBody1)

	errNoAllocations, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ibp-no-allocations")})
	assert.NotNil(t, errNoAllocations)

	errBodyNameClash, err := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ibp-3")})
	assert.Nil(t, err)

	// only name update
	okNameUpdateBody1, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ipb-updated")})
	assert.NotNil(t, okNameUpdateBody1)

	okNameUpdateBody1DifferentSite, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ibp-1")})
	assert.NotNil(t, okNameUpdateBody1DifferentSite)

	okNameUpdateBody2, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ipb-updated-new")})
	assert.NotNil(t, okNameUpdateBody2)

	okNameUpdateBody3, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ipb-updated-1"), Description: cutil.GetPtr("testdescription")})
	assert.NotNil(t, okNameUpdateBody3)

	okNameUpdateBody5, _ := json.Marshal(model.APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("test-ibp-1")})
	assert.NotNil(t, okNameUpdateBody5)

	// OTEL Spanner configuration
	tmc := &tmocks.Client{}
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site1.ID.String()] = tmc
	scp.IDClientMap[site2.ID.String()] = tmc
	scp.IDClientMap[site3.ID.String()] = tmc
	scp.IDClientMap[site4.ID.String()] = tmc

	wrunUpdate := &tmocks.WorkflowRun{}
	wrunUpdate.On("GetID").Return("test-workflow-update-id")
	wrunUpdate.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateInfiniBandPartition", mock.Anything).Return(wrunUpdate, nil)

	tests := []struct {
		name                     string
		reqOrgName               string
		user                     *cdbm.User
		ibpID                    string
		reqBody                  string
		expectedStatus           int
		expectedName             *string
		expectedSiteAssociations bool
		countSiteAssociations    int
		verifyChildSpanner       bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg2,
			user:           nil,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user is not a member of org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			user:           tnu3,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant is not admin for org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when ipb id doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			ibpID:          uuid.New().String(),
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when ipb tenant doesnt match tenant in org",
			reqOrgName:     tnOrg4,
			user:           tnu4,
			ibpID:          ibp1.ID.String(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when body does not bind",
			reqOrgName:     tnOrg1,
			reqBody:        "badbody",
			user:           tnu1,
			ibpID:          ibp1.ID.String(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when name clashes",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			ibpID:          ibp1.ID.String(),
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "error when tenant has no allocations at site",
			reqOrgName:     tnOrg1,
			reqBody:        string(errNoAllocations),
			user:           tnu5,
			ibpID:          ibp1.ID.String(),
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "success case when only name updated",
			reqOrgName:         tnOrg1,
			reqBody:            string(okNameUpdateBody1),
			user:               tnu1,
			ibpID:              ibp1.ID.String(),
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
			expectedName:       cutil.GetPtr("test-ipb-updated"),
		},
		{
			name:               "success case when same name updated but exists on different site",
			reqOrgName:         tnOrg5,
			reqBody:            string(okNameUpdateBody1DifferentSite),
			user:               tnu6,
			ibpID:              ibp5.ID.String(),
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
			expectedName:       cutil.GetPtr("test-ibp-1"),
		},
		{
			name:               "success case when name and description updated",
			reqOrgName:         tnOrg4,
			reqBody:            string(okNameUpdateBody3),
			user:               tnu4,
			ibpID:              ibp2.ID.String(),
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
			expectedName:       cutil.GetPtr("test-ipb-updated-1"),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.ibpID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			uibpgh := UpdateInfiniBandPartitionHandler{
				dbSession: dbSession,
				tc:        tmc,
				scp:       scp,
				cfg:       cfg,
			}
			err := uibpgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if rec.Code == http.StatusOK {
				rsp := &model.APIInfiniBandPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.ibpID, rsp.ID)
				if tc.expectedName != nil {
					assert.Equal(t, *tc.expectedName, rsp.Name)
				}
			}
			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfiniBandPartitionHandler_Delete(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
		scp       *sc.ClientPool
	}
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"
	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles1)
	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles2)
	tnu3 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg3}, tnRoles1)
	tnu4 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg4}, tnRoles1)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	tn2 := testFabricBuildTenant(t, dbSession, tnOrg4, "testTenant2")
	assert.NotNil(t, tn2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn2.ID, site2.ID, tnu4.ID)
	assert.NotNil(t, ts2)

	tn3 := testFabricBuildTenant(t, dbSession, tnOrg3, "testTenant3")
	assert.NotNil(t, tn3)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg3, tn3.ID, site2.ID, tnu3.ID)
	assert.NotNil(t, ts3)

	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg1, site1, tn1, nil, nil, false)
	assert.NotNil(t, ibp1)

	ibp2 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg2, site2, tn2, nil, nil, false)
	assert.NotNil(t, ibp2)

	ibp3 := testBuildIBPartition(t, dbSession, "test-ibp-3", tnOrg3, site2, tn3, nil, nil, false)
	assert.NotNil(t, ibp3)

	ibpBlocked := testBuildIBPartition(t, dbSession, "test-ibp-blocked", tnOrg1, site1, tn1, nil, nil, false)
	assert.NotNil(t, ibpBlocked)

	ipuIBDel := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, []string{authz.ProviderAdminRole})
	alIBDel := testInstanceSiteBuildAllocation(t, dbSession, site1, tn1, "test-allocation-ibp-delete", ipuIBDel)
	assert.NotNil(t, alIBDel)
	istIBDel := testInstanceBuildInstanceType(t, dbSession, ip1, "test-inst-type-ibp-delete", site1, cdbm.InstanceStatusReady)
	assert.NotNil(t, istIBDel)
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, alIBDel, cdbm.AllocationResourceTypeInstanceType, istIBDel.ID, cdbm.AllocationConstraintTypeReserved, 5, ipuIBDel)
	mcIBDel := testInstanceBuildMachine(t, dbSession, ip1.ID, site1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcIBDel)
	_ = testInstanceBuildMachineInstanceType(t, dbSession, mcIBDel, istIBDel)
	osIBDel := testInstanceBuildOperatingSystem(t, dbSession, "test-os-ibp-delete", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, osIBDel)
	vpcIBDel := testInstanceBuildVPC(t, dbSession, "test-vpc-ibp-delete", ip1, tn1, site1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpcIBDel)
	instIBDel := testInstanceBuildInstance(t, dbSession, "test-inst-ibp-delete", tn1.ID, ip1.ID, site1.ID, &istIBDel.ID, vpcIBDel.ID, cutil.GetPtr(mcIBDel.ID), &osIBDel.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instIBDel)
	_ = testInstanceBuildIBInterface(t, dbSession, instIBDel, site1, ibpBlocked, 0, false, cutil.GetPtr(1), cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}
	tcfg, _ := cfg.GetTemporalConfig()

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[site1.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInfiniBandPartitionV2", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[site2.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInfiniBandPartitionV2", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Prepare client pool for sync calls
	// to site(s).

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site1.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInfiniBandPartitionV2", mock.Anything).Return(wrun, nil)

	tests := []struct {
		fields               fields
		name                 string
		reqOrgName           string
		user                 *cdbm.User
		ibpID                string
		expectedErr          bool
		expectedStatus       int
		verifyChildSpanner   bool
		expectedErrorMessage string
	}{
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg2,
			user:           nil,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:           "error when user is not a member of org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			user:           tnu3,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:           "error when tenant is not admin for org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			ibpID:          *cutil.GetPtr(ibp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:           "error when ipb id doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			ibpID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:           "error when ipb tenant doesnt match tenant in org",
			reqOrgName:     tnOrg4,
			user:           tnu4,
			ibpID:          ibp1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:                 "error when active Instances are associated via InfiniBand Interfaces",
			reqOrgName:           tnOrg1,
			user:                 tnu1,
			ibpID:                ibpBlocked.ID.String(),
			expectedErr:          true,
			expectedStatus:       http.StatusBadRequest,
			expectedErrorMessage: "1 active Instances are associated with this InfiniBand Partition, unable to delete",
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:               "test InfiniBand Partition delete API endpoint success case",
			reqOrgName:         tnOrg1,
			user:               tnu1,
			ibpID:              ibp1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name: "test InfiniBand Partition delete API endpoint nico not-found, still success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoNotFound,
				scp:       scpWithNICoNotFound,
				cfg:       cfg,
			},
			reqOrgName:         tnOrg3,
			user:               tnu3,
			ibpID:              ibp3.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPost, "/", nil)

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.ibpID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			ibpdh := DeleteInfiniBandPartitionHandler{
				dbSession: tc.fields.dbSession,
				tc:        tc.fields.tc,
				scp:       tc.fields.scp,
				cfg:       tc.fields.cfg,
			}
			err := ibpdh.Handle(ec)
			assert.Nil(t, err)

			assert.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)

			if tc.expectedErrorMessage != "" {
				require.NotEmpty(t, rec.Body.Bytes())
				var payload struct {
					Message string `json:"message"`
				}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &payload))
				require.Equal(t, tc.expectedErrorMessage, payload.Message)
			}

			if !tc.expectedErr {
				// Check that InfiniBand Partition status is set to deleting
				ibpDAO := cdbm.NewInfiniBandPartitionDAO(dbSession)
				id1, err := uuid.Parse(tc.ibpID)
				assert.NoError(t, err)
				ibp, err := ibpDAO.GetByID(ctx, nil, id1, nil)
				assert.NoError(t, err)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusDeleting, ibp.Status)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
