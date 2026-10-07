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
	"strconv"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
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

func testBuildNVLinkLogicalPartition(t *testing.T, dbSession *cdb.Session, name string, description *string, org string, site *cdbm.Site, tenant *cdbm.Tenant, status *cdbm.NVLinkLogicalPartitionStatus, isMissingOnSite bool) *cdbm.NVLinkLogicalPartition {
	if status == nil {
		status = cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady)
	}
	nvllp := &cdbm.NVLinkLogicalPartition{
		ID:              uuid.New(),
		Name:            name,
		Description:     description,
		Org:             org,
		SiteID:          site.ID,
		TenantID:        tenant.ID,
		Status:          *status,
		IsMissingOnSite: isMissingOnSite,
	}
	_, err := dbSession.DB.NewInsert().Model(nvllp).Exec(context.Background())
	assert.Nil(t, err)
	return nvllp
}

func TestNVLinkLogicalPartitionHandler_Create(t *testing.T) {
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
	SiteConfig := &cdbm.SiteConfig{
		NVLinkPartition: true,
	}
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", SiteConfig, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	// This site should get no allocations to verify that
	// creation and updates fail when a tenant has no allocations
	// at a site.
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", SiteConfig, nil)
	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	site3 := testFabricBuildSite(t, dbSession, ip1, "testSite3", SiteConfig, nil)
	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site3.ID, tnu1.ID)
	assert.NotNil(t, ts3)

	al := testBuildAllocation(t, dbSession, site1, tn1, "test-allocation", tnu1)
	assert.NotNil(t, al)

	al2 := testBuildAllocation(t, dbSession, site3, tn1, "test-allocation-2", tnu1)
	assert.NotNil(t, al2)

	nvllpObj := model.APINVLinkLogicalPartitionCreateRequest{Name: "test-nvllp-1", Description: cutil.GetPtr("test"), SiteID: site1.ID.String()}
	okBody, err := json.Marshal(nvllpObj)
	assert.Nil(t, err)

	nvllpObj1 := model.APINVLinkLogicalPartitionCreateRequest{Name: "test-nvllp-2", Description: cutil.GetPtr("test")}
	errBodyMissingSite, err := json.Marshal(nvllpObj1)
	assert.Nil(t, err)

	nvllpObj4 := model.APINVLinkLogicalPartitionCreateRequest{Name: "test-nvllp-5", Description: cutil.GetPtr("test"), SiteID: "124"}
	errBodyInvalidSite, err := json.Marshal(nvllpObj4)
	assert.Nil(t, err)

	nvllpObj6 := model.APINVLinkLogicalPartitionCreateRequest{Name: "test-nvllp-6", Description: cutil.GetPtr("test"), SiteID: site3.ID.String()}
	okBody2, err := json.Marshal(nvllpObj6)
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(nvllpObj)
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	e := echo.New()
	cfg := common.GetTestConfig()

	// Mock per-Site client for site1
	tsc := &tmocks.Client{}

	// Mock per-Site client for site2
	tsc2 := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site1.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc
	scp.IDClientMap[site3.ID.String()] = tsc2

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	// Mock create call for CreateNVLinkLogicalPartition workflow
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateNVLinkLogicalPartition", mock.Anything).Return(wrun, nil)

	// Mock Get to populate workflow result
	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.NVLinkLogicalPartition)
			*response = &corev1.NVLinkLogicalPartition{
				Id:     &corev1.NVLinkLogicalPartitionId{Value: "test-nvllp-id"},
				Status: &corev1.NVLinkLogicalPartitionStatus{State: corev1.TenantState_READY},
			}
			return nil
		},
	)

	// Mock timeout error for timeout test case
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")
	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	// Mock timeout workflow execution
	tsc2.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateNVLinkLogicalPartition", mock.Anything).Return(wruntimeout, nil).Once()

	tsc2.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		fields             fields
		name               string
		reqOrgName         string
		reqBody            string
		reqBodyModel       *model.APINVLinkLogicalPartitionCreateRequest
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
			reqBodyModel:   &nvllpObj,
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
			reqBodyModel:   &nvllpObj,
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
			reqBodyModel:   &nvllpObj,
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
			name:               "success case with CreateNVLinkLogicalPartition workflow",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody),
			reqBodyModel:       &nvllpObj,
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
			name:               "timeout case with CreateNVLinkLogicalPartitionV2 workflow",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody2),
			reqBodyModel:       &nvllpObj6,
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
			name:           "error due to name clash",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			expectedStatus: http.StatusConflict,
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

			ec.SetRequest(ec.Request().WithContext(ctx))

			cibph := CreateNVLinkLogicalPartitionHandler{
				dbSession: tc.fields.dbSession,
				tc:        tc.fields.tc,
				scp:       scp,
				cfg:       tc.fields.cfg,
			}
			err := cibph.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if rec.Code == http.StatusCreated {
				rsp := &model.APINVLinkLogicalPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 2)
				assert.Equal(t, rsp.Name, tc.reqBodyModel.Name)
				if tc.reqBodyModel.Description != nil {
					assert.Equal(t, *tc.reqBodyModel.Description, *rsp.Description)
				}
				assert.Equal(t, rsp.Status, cdbm.NVLinkLogicalPartitionStatusReady)

				if len(tsc.Calls) > 0 {
					req := tsc.Calls[0].Arguments[3].(*corev1.NVLinkLogicalPartitionCreationRequest)
					assert.Equal(t, req.Config.Metadata.Name, tc.reqBodyModel.Name)
					if tc.reqBodyModel.Description != nil {
						assert.Equal(t, *tc.reqBodyModel.Description, req.Config.Metadata.Description)
					}
					assert.Equal(t, req.Config.TenantOrganizationId, tc.reqOrgName)
				}
			}
			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
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
	SiteConfig := &cdbm.SiteConfig{
		NVLinkPartition: true,
	}
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", SiteConfig, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	// This site should get no allocations to verify that
	// creation and updates fail when a tenant has no allocations
	// at a site.
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", SiteConfig, nil)
	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	site3 := testFabricBuildSite(t, dbSession, ip1, "testSite3", SiteConfig, nil)
	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site3.ID, tnu1.ID)
	assert.NotNil(t, ts3)

	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, site1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusPending), false)
	assert.NotNil(t, nvllp1)

	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, site2, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusPending), false)
	assert.NotNil(t, nvllp2)

	nvllp3 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-3", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg2, site1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusPending), false)
	assert.NotNil(t, nvllp3)

	nvllp4 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-4", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, site3, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusPending), false)
	assert.NotNil(t, nvllp4)

	nvllp5 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-5", cutil.GetPtr("preserved-for-nico"), tnOrg1, site1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusPending), false)
	assert.NotNil(t, nvllp5)

	noupdateObj := model.APINVLinkLogicalPartitionUpdateRequest{Name: cutil.GetPtr("test-nvllp-1"), Description: cutil.GetPtr("Test NVLink Logical Partition")}
	noupdateBody, err := json.Marshal(noupdateObj)
	assert.Nil(t, err)

	nvllpObj := model.APINVLinkLogicalPartitionUpdateRequest{Name: cutil.GetPtr("test-nvllp-updated-1"), Description: cutil.GetPtr("testdescription")}
	okBody, err := json.Marshal(nvllpObj)
	assert.Nil(t, err)

	nvllpObj6 := model.APINVLinkLogicalPartitionUpdateRequest{Name: cutil.GetPtr("test-nvllp-updated-6"), Description: cutil.GetPtr("testdescription")}
	okBody2, err := json.Marshal(nvllpObj6)
	assert.Nil(t, err)

	descOnlyObj := model.APINVLinkLogicalPartitionUpdateRequest{Description: cutil.GetPtr("updated description without name in body")}
	descOnlyBody, err := json.Marshal(descOnlyObj)
	assert.Nil(t, err)

	nameOnlyObj := model.APINVLinkLogicalPartitionUpdateRequest{Name: cutil.GetPtr("test-nvllp-5-renamed")}
	nameOnlyBody, err := json.Marshal(nameOnlyObj)
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(model.APINVLinkLogicalPartitionUpdateRequest{Name: cutil.GetPtr("test-nvllp-3")})
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(model.APINVLinkLogicalPartitionUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	cfg := common.GetTestConfig()

	// Mock per-Site client for site1
	tsc := &tmocks.Client{}

	// Mock per-Site client for site2
	tsc2 := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site1.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc
	scp.IDClientMap[site3.ID.String()] = tsc2

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	// Mock create call for CreateNVLinkLogicalPartitionV2 workflow
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateNVLinkLogicalPartition", mock.Anything).Return(wrun, nil)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	// Mock timeout error for timeout test case
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")
	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	// Mock timeout workflow execution
	tsc2.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateNVLinkLogicalPartition", mock.Anything).Return(wruntimeout, nil).Once()

	tsc2.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		fields                          fields
		name                            string
		reqOrgName                      string
		reqBody                         string
		reqBodyModel                    *model.APINVLinkLogicalPartitionUpdateRequest
		nvllpID                         string
		user                            *cdbm.User
		expectedStatus                  int
		expectedName                    bool
		verifyChildSpanner              bool
		verifyTemporalCall              bool
		expectedNICoMetadataName        string  // Metadata.Name on the site workflow request (always from DB after update)
		expectedNICoMetadataDescription *string // Metadata.Description when asserting NICo payload (DB snapshot after update)
	}{
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:           "error when user not found in request context",
			nvllpID:        nvllp1.ID.String(),
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody),
			reqBodyModel:   &nvllpObj,
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
			nvllpID:        nvllp1.ID.String(),
			reqBody:        string(okBody),
			reqBodyModel:   &nvllpObj,
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
			nvllpID:        nvllp1.ID.String(),
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody),
			reqBodyModel:   &nvllpObj,
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
			nvllpID:        nvllp1.ID.String(),
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
			nvllpID:        nvllp1.ID.String(),
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
			nvllpID:        nvllp1.ID.String(),
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
			name:           "error due to name clash",
			nvllpID:        nvllp1.ID.String(),
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			expectedStatus: http.StatusConflict,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:               "success case with UpdateNVLinkLogicalPartition workflow when there is no change",
			nvllpID:            nvllp1.ID.String(),
			reqOrgName:         tnOrg1,
			reqBody:            string(noupdateBody),
			reqBodyModel:       &noupdateObj,
			user:               tnu1,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
			verifyTemporalCall: false,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:               "success case with UpdateNVLinkLogicalPartition workflow",
			nvllpID:            nvllp1.ID.String(),
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody),
			reqBodyModel:       &nvllpObj,
			user:               tnu1,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
			verifyTemporalCall: true,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:                            "success case description-only update request sends current name to NICo",
			nvllpID:                         nvllp2.ID.String(),
			reqOrgName:                      tnOrg1,
			reqBody:                         string(descOnlyBody),
			reqBodyModel:                    &descOnlyObj,
			user:                            tnu1,
			expectedStatus:                  http.StatusOK,
			verifyChildSpanner:              true,
			expectedNICoMetadataName:        "test-nvllp-2",
			expectedNICoMetadataDescription: cutil.GetPtr("updated description without name in body"),
			verifyTemporalCall:              true,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			name:                            "success case name-only update request sends DB name and preserved description to NICo",
			nvllpID:                         nvllp5.ID.String(),
			reqOrgName:                      tnOrg1,
			reqBody:                         string(nameOnlyBody),
			reqBodyModel:                    &nameOnlyObj,
			user:                            tnu1,
			expectedStatus:                  http.StatusOK,
			verifyChildSpanner:              true,
			expectedNICoMetadataName:        "test-nvllp-5-renamed",
			expectedNICoMetadataDescription: cutil.GetPtr("preserved-for-nico"),
			verifyTemporalCall:              true,
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tsc2,
				cfg:       cfg,
			},
			name:               "timeout case with UpdateNVLinkLogicalPartition workflow",
			nvllpID:            nvllp4.ID.String(),
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody2),
			reqBodyModel:       &nvllpObj6,
			user:               tnu1,
			expectedStatus:     http.StatusInternalServerError,
			verifyChildSpanner: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.nvllpID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			uibph := UpdateNVLinkLogicalPartitionHandler{
				dbSession: tc.fields.dbSession,
				tc:        tc.fields.tc,
				scp:       scp,
				cfg:       tc.fields.cfg,
			}
			err := uibph.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if rec.Code == http.StatusOK {
				rsp := &model.APINVLinkLogicalPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				if tc.reqBodyModel.Name != nil {
					assert.Equal(t, *tc.reqBodyModel.Name, rsp.Name)
				}
				if tc.reqBodyModel.Description != nil {
					assert.Equal(t, *tc.reqBodyModel.Description, *rsp.Description)
				}

				var updateReq *corev1.NVLinkLogicalPartitionUpdateRequest
				for i := len(tsc.Calls) - 1; i >= 0; i-- {
					call := tsc.Calls[i]
					if call.Method == "ExecuteWorkflow" && len(call.Arguments) > 3 {
						if wfName, ok := call.Arguments[2].(string); ok && wfName == "UpdateNVLinkLogicalPartition" {
							updateReq, _ = call.Arguments[3].(*corev1.NVLinkLogicalPartitionUpdateRequest)
							break
						}
					}
				}

				if tc.verifyTemporalCall {
					require.NotNil(t, updateReq, "UpdateNVLinkLogicalPartition workflow should have been called")
				} else {
					require.Nil(t, updateReq, "UpdateNVLinkLogicalPartition workflow should not have been called")
					return
				}

				if tc.reqBodyModel.Description != nil || tc.reqBodyModel.Name != nil {
					if tc.expectedNICoMetadataName != "" {
						assert.Equal(t, tc.expectedNICoMetadataName, updateReq.Config.Metadata.Name)
					} else if tc.reqBodyModel.Name != nil {
						assert.Equal(t, *tc.reqBodyModel.Name, updateReq.Config.Metadata.Name)
					}
					if tc.expectedNICoMetadataDescription != nil {
						assert.Equal(t, *tc.expectedNICoMetadataDescription, updateReq.Config.Metadata.Description)
					} else if tc.reqBodyModel.Description != nil {
						assert.Equal(t, *tc.reqBodyModel.Description, updateReq.Config.Metadata.Description)
					}
					assert.Equal(t, tc.reqOrgName, updateReq.Config.TenantOrganizationId)
				}
			}
			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionHandler_GetAll(t *testing.T) {
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
	SiteConfig := &cdbm.SiteConfig{
		NVLinkPartition: true,
	}

	ipu := testFabricBuildUser(t, dbSession, "test-starfleet-id-1", []string{ipOrg1}, []string{authz.ProviderAdminRole})

	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", SiteConfig, cutil.GetPtr(cdbm.SiteStatusRegistered))

	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", SiteConfig, cutil.GetPtr(cdbm.SiteStatusRegistered))

	site3 := testFabricBuildSite(t, dbSession, ip1, "testSite3", SiteConfig, cutil.GetPtr(cdbm.SiteStatusRegistered))

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	tn2 := testFabricBuildTenant(t, dbSession, tnOrg4, "testTenant2")
	assert.NotNil(t, tn2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn2.ID, site2.ID, tnu4.ID)
	assert.NotNil(t, ts2)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn2.ID, site3.ID, tnu4.ID)
	assert.NotNil(t, ts3)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	al1 := testInstanceSiteBuildAllocation(t, dbSession, site1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip1, "test-instance-type-1", site1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	ist2 := testInstanceBuildInstanceType(t, dbSession, ip1, "test-instance-type-2", site2, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist2)

	mc1 := testInstanceBuildMachine(t, dbSession, ip1.ID, site1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	mc2 := testInstanceBuildMachine(t, dbSession, ip1.ID, site2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)

	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist2)
	assert.NotNil(t, mcinst2)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(dbSession)

	totalCount := 30

	nvllps := []cdbm.NVLinkLogicalPartition{}

	for i := 0; i < totalCount; i++ {
		nvllp, err := nvllpDAO.Create(
			ctx,
			nil,
			cdbm.NVLinkLogicalPartitionCreateInput{
				Name:        fmt.Sprintf("test-nvllp-%02d", i),
				Description: cutil.GetPtr("test"),
				TenantOrg:   tnOrg1,
				SiteID:      site1.ID,
				TenantID:    tn1.ID,
				Status:      cdbm.NVLinkLogicalPartitionStatusPending,
				CreatedBy:   tnu1.ID,
			},
		)
		assert.Nil(t, err)
		common.TestBuildStatusDetail(t, dbSession, nvllp.ID.String(), string(cdbm.NVLinkLogicalPartitionStatusPending), cutil.GetPtr("request received, pending processing"))
		common.TestBuildStatusDetail(t, dbSession, nvllp.ID.String(), string(cdbm.NVLinkLogicalPartitionStatusReady), cutil.GetPtr("NVLinkLogical Partition is now ready for use"))
		nvllps = append(nvllps, *nvllp)
	}

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip1, tn1, site1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllps[0].ID), cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	//Site 2

	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1-site2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg4, site2, tn2, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	common.TestBuildStatusDetail(t, dbSession, nvllp1.ID.String(), string(cdbm.NVLinkLogicalPartitionStatusPending), cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, nvllp1.ID.String(), string(cdbm.NVLinkLogicalPartitionStatusReady), cutil.GetPtr("NVLinkLogical Partition is now ready for use"))

	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2-site2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg4, site2, tn2, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp2)

	common.TestBuildStatusDetail(t, dbSession, nvllp2.ID.String(), string(cdbm.NVLinkLogicalPartitionStatusPending), cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, nvllp2.ID.String(), string(cdbm.NVLinkLogicalPartitionStatusReady), cutil.GetPtr("NVLinkLogical Partition is now ready for use"))

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip1, tn2, site2, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllp1.ID), cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip1.ID, site1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	inst2 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn2.ID, ip1.ID, site2.ID, &ist2.ID, vpc2.ID, cutil.GetPtr(mc2.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst2)

	// Create NVLinkInterface records for each NVLinkLogicalPartition

	nvlifc1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site1.ID, inst1.ID, nvllps[0].ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc1)

	nvlifc2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site2.ID, inst2.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc2)

	nvlifc3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site1.ID, inst1.ID, nvllps[0].ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc3)

	nvlifc4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site2.ID, inst2.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc4)

	nvlifc5 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site1.ID, inst1.ID, nvllps[0].ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc5)

	nvlifc6 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site2.ID, inst2.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc6)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                     string
		reqOrgName               string
		user                     *cdbm.User
		querySiteID              *string
		querySearch              *string
		queryStatus              *string
		queryIncludeRelations1   *string
		queryIncludeRelations2   *string
		queryIncludeRelations3   *string
		queryIncludeInterfaces   *bool
		queryIncludeVpcs         *bool
		pageNumber               *int
		pageSize                 *int
		orderBy                  *string
		expectedErr              bool
		expectedStatus           int
		expectedCnt              int
		expectedTotal            *int
		expectedFirstEntry       *cdbm.NVLinkLogicalPartition
		expectedTenantOrg        *string
		expectedSite             *cdbm.Site
		expectedNVLinkInterfaces []cdbm.NVLinkInterface
		expectedVpcs             []cdbm.Vpc
		verifyChildSpanner       bool
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
			name:                     "success case when objects returned",
			reqOrgName:               tnOrg1,
			user:                     tnu1,
			expectedErr:              false,
			queryIncludeInterfaces:   cutil.GetPtr(true),
			queryIncludeVpcs:         cutil.GetPtr(true),
			expectedNVLinkInterfaces: []cdbm.NVLinkInterface{*nvlifc1, *nvlifc3, *nvlifc5},
			expectedVpcs:             []cdbm.Vpc{*vpc1},
			expectedStatus:           http.StatusOK,
			expectedCnt:              paginator.DefaultLimit,
			expectedTotal:            &totalCount,
			verifyChildSpanner:       true,
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
			querySiteID:    cutil.GetPtr(site3.ID.String()),
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
			expectedFirstEntry: &nvllps[29],
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
			name:           "success when NVLinkLogicalPartitionStatusPending status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			querySiteID:    cutil.GetPtr(site1.ID.String()),
			queryStatus:    cdb.GetTypedStrPtr(cdbm.NVLinkLogicalPartitionStatusPending),
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
			if tc.queryIncludeVpcs != nil {
				q.Add("includeVpcs", fmt.Sprintf("%v", *tc.queryIncludeVpcs))
			}
			if tc.queryIncludeInterfaces != nil {
				q.Add("includeInterfaces", fmt.Sprintf("%v", *tc.queryIncludeInterfaces))
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

			path := fmt.Sprintf("/v2/org/%s/nico/nvlinklogical-partition?%s", tc.reqOrgName, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			ibpah := GetAllNVLinkLogicalPartitionHandler{
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
				rsp := []model.APINVLinkLogicalPartition{}
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

				if tc.queryIncludeInterfaces != nil {
					assert.Equal(t, len(tc.expectedNVLinkInterfaces), len(rsp[0].NVLinkInterfaces))
				}

				if tc.queryIncludeVpcs != nil {
					assert.Equal(t, len(tc.expectedVpcs), len(rsp[0].Vpcs))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionHandler_GetByID(t *testing.T) {
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
	SiteConfig := &cdbm.SiteConfig{
		NVLinkPartition: true,
	}

	ipu := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, []string{authz.ProviderAdminRole})

	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", SiteConfig, cutil.GetPtr(cdbm.SiteStatusRegistered))

	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", SiteConfig, cutil.GetPtr(cdbm.SiteStatusRegistered))

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

	al1 := testInstanceSiteBuildAllocation(t, dbSession, site1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip1, "test-instance-type-1", site1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	ist2 := testInstanceBuildInstanceType(t, dbSession, ip1, "test-instance-type-2", site2, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist2)

	mc1 := testInstanceBuildMachine(t, dbSession, ip1.ID, site1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	mc2 := testInstanceBuildMachine(t, dbSession, ip1.ID, site2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)

	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist2)
	assert.NotNil(t, mcinst2)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, site1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg2, site2, tn2, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp2)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip1, tn1, site1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllp1.ID), cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip1, tn1, site2, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllp2.ID), cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip1.ID, site1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	inst2 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip1.ID, site2.ID, &ist2.ID, vpc2.ID, cutil.GetPtr(mc2.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst2)

	nvlifc1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site1.ID, inst1.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc1)

	nvlifc2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, site2.ID, inst2.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, nvlifc2)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                           string
		reqOrgName                     string
		user                           *cdbm.User
		nvllpID                        string
		queryIncludeRelations1         *string
		queryIncludeRelations2         *string
		queryIncludeRelations3         *string
		queryIncludeInterfaces         *bool
		queryIncludeVpcs               *bool
		expectedNVLinkInterfaces       []cdbm.NVLinkInterface
		expectedVpcs                   []cdbm.Vpc
		expectedErr                    bool
		expectedStatus                 int
		expectedTenantOrg              *string
		expectedSite                   *cdbm.Site
		expectedNVLinkLogicalPartition *cdbm.NVLinkLogicalPartition
		verifyChildSpanner             bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg2,
			user:           nil,
			nvllpID:        *cutil.GetPtr(nvllp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user is not a member of org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			nvllpID:        *cutil.GetPtr(nvllp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			user:           tnu3,
			nvllpID:        *cutil.GetPtr(nvllp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant is not admin for org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			nvllpID:        *cutil.GetPtr(nvllp1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when partition id doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			nvllpID:        uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when partition's tenant doesnt match tenant in org",
			reqOrgName:     tnOrg4,
			user:           tnu4,
			nvllpID:        nvllp1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                           "success case 1",
			reqOrgName:                     tnOrg1,
			user:                           tnu1,
			nvllpID:                        nvllp1.ID.String(),
			expectedErr:                    false,
			expectedStatus:                 http.StatusOK,
			expectedNVLinkLogicalPartition: nvllp1,
			queryIncludeInterfaces:         cutil.GetPtr(true),
			expectedNVLinkInterfaces:       []cdbm.NVLinkInterface{*nvlifc1},
			verifyChildSpanner:             true,
		},
		{
			name:                           "success case 2",
			reqOrgName:                     tnOrg4,
			user:                           tnu4,
			nvllpID:                        nvllp2.ID.String(),
			expectedErr:                    false,
			expectedStatus:                 http.StatusOK,
			expectedNVLinkLogicalPartition: nvllp2,
			queryIncludeInterfaces:         cutil.GetPtr(true),
			expectedNVLinkInterfaces:       []cdbm.NVLinkInterface{*nvlifc2},
			queryIncludeVpcs:               cutil.GetPtr(true),
			expectedVpcs:                   []cdbm.Vpc{*vpc2},
			verifyChildSpanner:             true,
		},
		{
			name:                   "success when site relation is specified",
			reqOrgName:             tnOrg1,
			user:                   tnu1,
			nvllpID:                nvllp1.ID.String(),
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

			if tc.queryIncludeInterfaces != nil {
				q.Add("includeInterfaces", strconv.FormatBool(*tc.queryIncludeInterfaces))
			}

			if tc.queryIncludeVpcs != nil {
				q.Add("includeVpcs", strconv.FormatBool(*tc.queryIncludeVpcs))
			}

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.nvllpID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			nvllpgh := GetNVLinkLogicalPartitionHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := nvllpgh.Handle(ec)
			assert.Nil(t, err)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			assert.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if !tc.expectedErr {
				rsp := &model.APINVLinkLogicalPartition{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				if tc.expectedNVLinkLogicalPartition != nil {
					assert.Equal(t, tc.expectedNVLinkLogicalPartition.Name, rsp.Name)
					assert.Equal(t, tc.expectedNVLinkLogicalPartition.TenantID.String(), rsp.TenantID)
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

				if tc.queryIncludeInterfaces != nil {
					assert.Equal(t, len(tc.expectedNVLinkInterfaces), len(rsp.NVLinkInterfaces))
					for i, nvllifc := range rsp.NVLinkInterfaces {
						assert.Equal(t, tc.expectedNVLinkInterfaces[i].ID.String(), nvllifc.ID)
						assert.Equal(t, tc.expectedNVLinkInterfaces[i].InstanceID.String(), nvllifc.InstanceID)
						assert.Equal(t, tc.expectedNVLinkInterfaces[i].NVLinkLogicalPartitionID.String(), nvllifc.NVLinkLogicalPartitionID)
					}
				}

				if tc.queryIncludeVpcs != nil {
					assert.Equal(t, len(tc.expectedVpcs), len(rsp.Vpcs))
					for i, vpc := range rsp.Vpcs {
						assert.Equal(t, tc.expectedVpcs[i].ID.String(), vpc.ID)
					}
				}

			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionHandler_Delete(t *testing.T) {
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
	SiteConfig := &cdbm.SiteConfig{
		NVLinkPartition: true,
	}
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", SiteConfig, nil)
	site2 := testFabricBuildSite(t, dbSession, ip1, "testSite2", SiteConfig, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	tn3 := testFabricBuildTenant(t, dbSession, tnOrg3, "testTenant3")
	assert.NotNil(t, tn3)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg3, tn3.ID, site2.ID, tnu3.ID)
	assert.NotNil(t, ts3)

	tn4 := testFabricBuildTenant(t, dbSession, tnOrg4, "testTenant4")
	assert.NotNil(t, tn4)

	ts4 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tn4.ID, site2.ID, tnu4.ID)
	assert.NotNil(t, ts4)

	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, site1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	nvllp3 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-3", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg3, site2, tn3, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp3)

	nvllp4 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-4", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg4, site2, tn4, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp4)

	ipuNvDel := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, []string{authz.ProviderAdminRole})
	alNvDel := testInstanceSiteBuildAllocation(t, dbSession, site2, tn4, "test-allocation-nvllp-delete", ipuNvDel)
	assert.NotNil(t, alNvDel)
	istNvDel := testInstanceBuildInstanceType(t, dbSession, ip1, "test-inst-type-nvllp-delete", site2, cdbm.InstanceStatusReady)
	assert.NotNil(t, istNvDel)
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, alNvDel, cdbm.AllocationResourceTypeInstanceType, istNvDel.ID, cdbm.AllocationConstraintTypeReserved, 5, ipuNvDel)
	mcNvDel := testInstanceBuildMachine(t, dbSession, ip1.ID, site2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcNvDel)
	_ = testInstanceBuildMachineInstanceType(t, dbSession, mcNvDel, istNvDel)
	osNvDel := testInstanceBuildOperatingSystem(t, dbSession, "test-os-nvllp-delete", tn4, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu4)
	assert.NotNil(t, osNvDel)
	vpcNvDel := testInstanceBuildVPC(t, dbSession, "test-vpc-nvllp-delete", ip1, tn4, site2, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu4)
	assert.NotNil(t, vpcNvDel)
	instNvDel := testInstanceBuildInstance(t, dbSession, "test-inst-nvllp-delete", tn4.ID, ip1.ID, site2.ID, &istNvDel.ID, vpcNvDel.ID, cutil.GetPtr(mcNvDel.ID), &osNvDel.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instNvDel)
	_ = testInstanceBuildInstanceNVLinkInterface(t, dbSession, site2.ID, instNvDel.ID, nvllp4.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

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
		"DeleteNVLinkLogicalPartition", mock.Anything).Return(wrunTimeout, nil)

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
		"DeleteNVLinkLogicalPartition", mock.Anything).Return(wrunWithNICoNotFound, nil)

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
		"DeleteNVLinkLogicalPartition", mock.Anything).Return(wrun, nil)

	tests := []struct {
		fields               fields
		name                 string
		reqOrgName           string
		user                 *cdbm.User
		nvllpID              string
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
			nvllpID:        nvllp1.ID.String(),
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
			nvllpID:        nvllp1.ID.String(),
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
			name:           "error when user doesn't have Tenant Admin role with org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			nvllpID:        nvllp1.ID.String(),
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
			name:           "error when tenant is not admin for org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			nvllpID:        nvllp1.ID.String(),
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
			name:           "error when partition id doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			nvllpID:        uuid.New().String(),
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
			name:           "error when partition is not owned by current org's Tenant",
			reqOrgName:     tnOrg4,
			user:           tnu4,
			nvllpID:        nvllp1.ID.String(),
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
			name:                 "error when active Instances are associated via NVLink Interfaces",
			reqOrgName:           tnOrg4,
			user:                 tnu4,
			nvllpID:              nvllp4.ID.String(),
			expectedErr:          true,
			expectedStatus:       http.StatusBadRequest,
			expectedErrorMessage: "1 active Instances are associated with this NVLink Logical Partition, unable to delete",
		},
		{
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			name:               "test NVLinkLogical Partition delete API endpoint success case",
			reqOrgName:         tnOrg1,
			user:               tnu1,
			nvllpID:            nvllp1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name: "test NVLinkLogical Partition delete API endpoint nico not-found, still success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoNotFound,
				scp:       scpWithNICoNotFound,
				cfg:       cfg,
			},
			reqOrgName:         tnOrg3,
			user:               tnu3,
			nvllpID:            nvllp3.ID.String(),
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
			values := []string{tc.reqOrgName, tc.nvllpID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			ibpdh := DeleteNVLinkLogicalPartitionHandler{
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
				// Check that NVLinkLogical Partition status is set to deleting
				nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(dbSession)
				nvllpID, err := uuid.Parse(tc.nvllpID)
				assert.NoError(t, err)
				nvllp, err := nvllpDAO.GetByID(ctx, nil, nvllpID, nil)
				assert.NoError(t, err)
				assert.Equal(t, cdbm.NVLinkLogicalPartitionStatusDeleting, nvllp.Status)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
