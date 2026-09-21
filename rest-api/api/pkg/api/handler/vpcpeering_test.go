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
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestNewGetVpcPeeringHandler(t *testing.T) {
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	got := NewGetVpcPeeringHandler(dbSession, tc, cfg)
	assert.Equal(t, dbSession, got.dbSession)
	assert.Equal(t, tc, got.tc)
	assert.Equal(t, cfg, got.cfg)
}

func TestNewDeleteVpcPeeringHandler(t *testing.T) {
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	tc := &tmocks.Client{}

	got := NewDeleteVpcPeeringHandler(dbSession, tc, scp, cfg)
	assert.Equal(t, dbSession, got.dbSession)
	assert.Equal(t, tc, got.tc)
	assert.Equal(t, scp, got.scp)
	assert.Equal(t, cfg, got.cfg)
}

func TestCreateVpcPeeringHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}
	ipOrg2 := "test-provider-tenant-org"
	ipOrgRoles2 := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	ipu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg2, ipOrgRoles2)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider-2", ipOrg2, ipu2)

	// Create two sites and update them to Registered status
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)
	st1.Status = cdbm.SiteStatusRegistered
	_, err := dbSession.DB.NewUpdate().Model(st1).Where("id = ?", st1.ID).Exec(context.Background())
	assert.Nil(t, err)

	st2 := common.TestBuildSite(t, dbSession, ip2, "test-site-2", ipu2)
	st2.Status = cdbm.SiteStatusRegistered
	_, err = dbSession.DB.NewUpdate().Model(st2).Where("id = ?", st2.ID).Exec(context.Background())
	assert.Nil(t, err)

	// Build tenants for multi-tenant peering test
	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)
	t1s1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, t1s1)
	t1s2 := common.TestBuildTenantSite(t, dbSession, tn1, st2, tnu1)
	assert.NotNil(t, t1s2)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)
	t2s1 := common.TestBuildTenantSite(t, dbSession, tn2, st1, tnu2)
	assert.NotNil(t, t2s1)
	t2s2 := common.TestBuildTenantSite(t, dbSession, tn2, st2, tnu2)
	assert.NotNil(t, t2s2)

	// Tenant tied to the dual-role (provider+tenant admin) org.
	tnProvider := common.TestBuildTenant(t, dbSession, "test-tenant-provider", ipOrg2, ipu2)
	common.TestBuildTenantSite(t, dbSession, tnProvider, st1, ipu2)

	ta1 := common.TestBuildTenantAccount(t, dbSession, ip, &tn1.ID, tn1.Org, cdbm.TenantAccountStatusReady, ipu)
	assert.NotNil(t, ta1)
	ta2 := common.TestBuildTenantAccount(t, dbSession, ip, &tn2.ID, tn2.Org, cdbm.TenantAccountStatusReady, ipu)
	assert.NotNil(t, ta2)

	ta3 := common.TestBuildTenantAccount(t, dbSession, ip2, &tn1.ID, tn1.Org, cdbm.TenantAccountStatusReady, ipu2)
	assert.NotNil(t, ta3)
	ta4 := common.TestBuildTenantAccount(t, dbSession, ip2, &tn2.ID, tn2.Org, cdbm.TenantAccountStatusReady, ipu2)
	assert.NotNil(t, ta4)

	// VPCs must be in Ready state for create peering to succeed
	vpc1 := common.TestBuildVPC(t, dbSession, "vpc-1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc2 := common.TestBuildVPC(t, dbSession, "vpc-2", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc3 := common.TestBuildVPC(t, dbSession, "vpc-3", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc4 := common.TestBuildVPC(t, dbSession, "vpc-4", ip, tn2, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc5 := common.TestBuildVPC(t, dbSession, "vpc-5", ip2, tnProvider, st1, nil, nil, nil, cdbm.VpcStatusReady, ipu2)
	vpc6 := common.TestBuildVPC(t, dbSession, "vpc-6", ip2, tnProvider, st1, nil, nil, nil, cdbm.VpcStatusReady, ipu2)
	vpc7 := common.TestBuildVPC(t, dbSession, "vpc-7", ip2, tn1, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc8 := common.TestBuildVPC(t, dbSession, "vpc-8", ip2, tn2, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu2)

	// Existing peering vpc1-vpc2 for duplicate test
	existingVP := common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc2.ID, st1.ID, nil, &tn1.ID, false, tnu1.ID)
	assert.NotNil(t, existingVP)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal client
	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateVpcPeering", mock.Anything).Return(mockWorkflowRun, nil)

	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
			st2.ID.String(): mockTC,
		},
	}

	// Success case: create vpc1-vpc3 peering (no existing peering between them)
	okBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc1.ID.String(),
		Vpc2ID: vpc3.ID.String(),
		SiteID: st1.ID.String(),
	}
	okBodyBytes, _ := json.Marshal(okBody)

	// Duplicate: vpc1-vpc2 already exists
	dupBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc1.ID.String(),
		Vpc2ID: vpc2.ID.String(),
		SiteID: st1.ID.String(),
	}
	dupBodyBytes, _ := json.Marshal(dupBody)

	// Invalid VPC ID
	invalidVpcIDBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: "invalid-uuid",
		Vpc2ID: vpc2.ID.String(),
		SiteID: st1.ID.String(),
	}
	invalidVpcIDBodyBytes, _ := json.Marshal(invalidVpcIDBody)

	invalidSiteBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc1.ID.String(),
		Vpc2ID: vpc2.ID.String(),
		SiteID: uuid.New().String(),
	}
	invalidSiteBodyBytes, _ := json.Marshal(invalidSiteBody)

	selfPeerBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc1.ID.String(),
		Vpc2ID: vpc1.ID.String(),
		SiteID: st1.ID.String(),
	}
	selfPeerBodyBytes, _ := json.Marshal(selfPeerBody)

	invalidBodyBytes := []byte(`{"vpc1": "x"}`)

	multiTenantBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc1.ID.String(),
		Vpc2ID: vpc4.ID.String(),
		SiteID: st1.ID.String(),
	}
	multiTenantBodyBytes, _ := json.Marshal(multiTenantBody)
	dualRoleTenantBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc5.ID.String(),
		Vpc2ID: vpc6.ID.String(),
		SiteID: st1.ID.String(),
	}
	dualRoleTenantBodyBytes, _ := json.Marshal(dualRoleTenantBody)

	dualRoleProviderBody := model.APIVpcPeeringCreateRequest{
		Vpc1ID: vpc7.ID.String(),
		Vpc2ID: vpc8.ID.String(),
		SiteID: st2.ID.String(),
	}
	dualRoleProviderBodyBytes, _ := json.Marshal(dualRoleProviderBody)

	tests := []struct {
		name           string
		reqOrgName     string
		reqBody        string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		expectedVpcIDs []string
		expectedSiteID string
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyBytes),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when org is not found",
			reqOrgName:     "SomeOtherOrg",
			reqBody:        string(okBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user does not belong to org",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyBytes),
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body is invalid",
			reqOrgName:     tnOrg1,
			reqBody:        string(invalidBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site does not exist",
			reqOrgName:     tnOrg1,
			reqBody:        string(invalidSiteBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when VPC ID in body is invalid",
			reqOrgName:     tnOrg1,
			reqBody:        string(invalidVpcIDBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when peering VPC with self",
			reqOrgName:     tnOrg1,
			reqBody:        string(selfPeerBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when peering already exists",
			reqOrgName:     tnOrg1,
			reqBody:        string(dupBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "error when Tenant Admin tries to create multi-tenant peering",
			reqOrgName:     tnOrg1,
			reqBody:        string(multiTenantBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when Provider Admin tries to create single tenant peering",
			reqOrgName:     ipOrg,
			reqBody:        string(okBodyBytes),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "ok when Tenant Admin creates single tenant peering",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyBytes),
			user:           tnu1,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedVpcIDs: []string{vpc1.ID.String(), vpc3.ID.String()},
			expectedSiteID: st1.ID.String(),
		},
		{
			name:           "ok when Provider Admin creates multi-tenant peering",
			reqOrgName:     ipOrg,
			reqBody:        string(multiTenantBodyBytes),
			user:           ipu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedVpcIDs: []string{vpc1.ID.String(), vpc4.ID.String()},
			expectedSiteID: st1.ID.String(),
		},
		{
			name:           "ok when user has both provider and tenant admin roles and creates single-tenant peering",
			reqOrgName:     ipOrg2,
			reqBody:        string(dualRoleTenantBodyBytes),
			user:           ipu2,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedVpcIDs: []string{vpc5.ID.String(), vpc6.ID.String()},
			expectedSiteID: st1.ID.String(),
		},
		{
			name:           "ok when user has both provider and tenant admin roles and creates multi-tenant peering",
			reqOrgName:     ipOrg2,
			reqBody:        string(dualRoleProviderBodyBytes),
			user:           ipu2,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedVpcIDs: []string{vpc7.ID.String(), vpc8.ID.String()},
			expectedSiteID: st2.ID.String(),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cvph := CreateVpcPeeringHandler{
				dbSession: dbSession,
				tc:        mockTC,
				scp:       mockSCP,
				cfg:       common.GetTestConfig(),
			}

			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tt.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrgName)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := cvph.Handle(ec)
			require.NoError(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && tt.expectedStatus == http.StatusCreated {
				var apiVpcPeering model.APIVpcPeering
				err := json.Unmarshal(rec.Body.Bytes(), &apiVpcPeering)
				require.NoError(t, err)
				assert.True(t,
					(apiVpcPeering.Vpc1ID == tt.expectedVpcIDs[0] && apiVpcPeering.Vpc2ID == tt.expectedVpcIDs[1]) ||
						(apiVpcPeering.Vpc1ID == tt.expectedVpcIDs[1] && apiVpcPeering.Vpc2ID == tt.expectedVpcIDs[0]),
					"expected vpc1Id and vpc2Id should match the list of expected VPC IDs")
				assert.True(t, apiVpcPeering.SiteID == tt.expectedSiteID, "expected siteId should match the expected site ID")
				assert.Equal(t, cdbm.VpcPeeringStatusReady, apiVpcPeering.Status, "expected status should be Ready")
			}
		})
	}
}

func TestGetAllVpcPeeringHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	ipOrg2 := "test-provider-org-2"
	ipOrgRoles2 := []string{authz.ProviderAdminRole, authz.TenantAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	// Provider users
	// ip is a provider admin, ip2 is a provider tenant admin
	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider-1", ipOrg, ipu)
	ipu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg2, ipOrgRoles2)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider-2", ipOrg2, ipu2)

	// Sites
	st1 := common.TestBuildSite(t, dbSession, ip, "site-1", ipu)
	st2 := common.TestBuildSite(t, dbSession, ip2, "site-2", ipu2)
	st3 := common.TestBuildSite(t, dbSession, ip2, "site-3", ipu2)
	for _, st := range []*cdbm.Site{st1, st2, st3} {
		st.Status = cdbm.SiteStatusRegistered
		_, err := dbSession.DB.NewUpdate().Model(st).Where("id = ?", st.ID).Exec(context.Background())
		assert.Nil(t, err)
	}

	// Tenants/users
	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "tenant-1", tnOrg1, tnu1)
	common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	common.TestBuildTenantSite(t, dbSession, tn1, st2, tnu1)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "tenant-2", tnOrg2, tnu2)
	common.TestBuildTenantSite(t, dbSession, tn2, st1, tnu2)
	common.TestBuildTenantSite(t, dbSession, tn2, st2, tnu2)
	common.TestBuildTenantSite(t, dbSession, tn2, st3, tnu2)

	// Tenant for provider+tenant admin user in provider org.
	// ipu2 is both the provider admin of site 2 and 3, and tenant admin of site 1
	tnProvider := common.TestBuildTenant(t, dbSession, "tenant-provider", ipOrg2, ipu2)
	common.TestBuildTenantSite(t, dbSession, tnProvider, st1, ipu2)

	// VPCs
	vpc1 := common.TestBuildVPC(t, dbSession, "vpc-1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc2 := common.TestBuildVPC(t, dbSession, "vpc-2", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc3 := common.TestBuildVPC(t, dbSession, "vpc-3", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc4 := common.TestBuildVPC(t, dbSession, "vpc-4", ip, tn2, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc5 := common.TestBuildVPC(t, dbSession, "vpc-5", ip, tn1, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc6 := common.TestBuildVPC(t, dbSession, "vpc-6", ip, tn2, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc7 := common.TestBuildVPC(t, dbSession, "vpc-7", ip, tnProvider, st1, nil, nil, nil, cdbm.VpcStatusReady, ipu2)
	vpc8 := common.TestBuildVPC(t, dbSession, "vpc-8", ip, tnProvider, st1, nil, nil, nil, cdbm.VpcStatusReady, ipu2)

	// Tenant-created peerings
	vp12 := common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc2.ID, st1.ID, nil, &tn1.ID, false, tnu1.ID)
	_ = common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc3.ID, st1.ID, nil, &tn1.ID, false, tnu1.ID)
	_ = common.TestBuildVpcPeering(t, dbSession, vpc7.ID, vpc8.ID, st1.ID, nil, &tnProvider.ID, false, ipu2.ID)

	// Provider-created peerings
	_ = common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc4.ID, st1.ID, &ip.ID, nil, true, ipu.ID)
	_ = common.TestBuildVpcPeering(t, dbSession, vpc2.ID, vpc4.ID, st1.ID, &ip.ID, nil, true, ipu.ID)
	_ = common.TestBuildVpcPeering(t, dbSession, vpc3.ID, vpc4.ID, st1.ID, &ip.ID, nil, true, ipu.ID)
	_ = common.TestBuildVpcPeering(t, dbSession, vpc5.ID, vpc6.ID, st2.ID, &ip2.ID, nil, true, ipu2.ID)

	vpPeeringDAO := cdbm.NewVpcPeeringDAO(dbSession)
	err := vpPeeringDAO.UpdateStatusByID(ctx, nil, vp12.ID, cdbm.VpcPeeringStatusReady)
	require.NoError(t, err)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)
	mockTC := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name                     string
		reqOrgName               string
		queryParams              map[string]string
		queryString              string
		user                     *cdbm.User
		expectedStatus           int
		expectedCount            int
		validatePagination       bool
		validateTenantIDs        bool
		validateTenantIDsPresent bool
		requirePeerTenantIDs     []uuid.UUID
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"siteId": st1.ID.String()},
			user:           nil,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user does not belong to org",
			reqOrgName:     "SomeOtherOrg",
			user:           tnu1,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg1,
			user:           tnu1Forbidden,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when siteId is invalid",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"siteId": "not-a-uuid"},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when siteId is empty",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"siteId": ""},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when isMultiTenant is empty",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"isMultiTenant": ""},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when siteId does not exist",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"siteId": uuid.New().String()},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "tenant admin 1 lists peerings in site 1",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"siteId": st1.ID.String()},
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedCount:  5,
		},
		{
			name:               "tenant admin 2 lists across all sites when siteId omitted",
			reqOrgName:         tnOrg2,
			user:               tnu2,
			expectedStatus:     http.StatusOK,
			expectedCount:      4,
			validatePagination: true,
		},
		{
			name:               "tenant admin 2 lists peerings in site 1",
			reqOrgName:         tnOrg2,
			queryParams:        map[string]string{"siteId": st1.ID.String()},
			user:               tnu2,
			expectedStatus:     http.StatusOK,
			expectedCount:      3,
			validatePagination: true,
		},
		{
			name:               "tenant admin 2 lists peerings in site 2",
			reqOrgName:         tnOrg2,
			queryParams:        map[string]string{"siteId": st2.ID.String()},
			user:               tnu2,
			expectedStatus:     http.StatusOK,
			expectedCount:      1,
			validatePagination: true,
		},
		{
			name:           "tenant admin 1 error list peerings in site 3",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"siteId": st3.ID.String()},
			user:           tnu1,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin 1 lists provider-created peerings across all sites",
			reqOrgName:     ipOrg,
			user:           ipu,
			expectedStatus: http.StatusOK,
			expectedCount:  3,
		},
		{
			name:           "provider admin 1 lists provider-created peerings in site 1",
			reqOrgName:     ipOrg,
			queryParams:    map[string]string{"siteId": st1.ID.String()},
			user:           ipu,
			expectedStatus: http.StatusOK,
			expectedCount:  3,
		},
		{
			name:           "provider admin 1 error when not authorized to access site 2",
			reqOrgName:     ipOrg,
			queryParams:    map[string]string{"siteId": st2.ID.String()},
			user:           ipu,
			expectedStatus: http.StatusForbidden,
			expectedCount:  0,
		},
		{
			name:           "provider admin forbidden for other provider site",
			reqOrgName:     ipOrg,
			queryParams:    map[string]string{"siteId": st3.ID.String()},
			user:           ipu,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider and tenant admin lists all peerings across all sites",
			reqOrgName:     ipOrg2,
			user:           ipu2,
			expectedStatus: http.StatusOK,
			expectedCount:  2,
		},
		{
			name:           "provider and tenant admin lists peering in site 1",
			reqOrgName:     ipOrg2,
			queryParams:    map[string]string{"siteId": st1.ID.String()},
			user:           ipu2,
			expectedStatus: http.StatusOK,
			expectedCount:  1,
		},
		{
			name:           "error when status query value is invalid",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"status": "NotAStatus"},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "tenant admin 1 filters by ready status",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"status": cdbm.VpcPeeringStatusReady},
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedCount:  1,
		},
		{
			name:           "tenant admin 1 filters by pending status",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"status": cdbm.VpcPeeringStatusPending},
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedCount:  5,
		},
		{
			name:           "error when vpcId is invalid",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"vpcId": "not-a-uuid"},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpcId does not exist",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"vpcId": uuid.New().String()},
			user:           tnu1,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "tenant admin 1 filters by vpcId",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"vpcId": vpc1.ID.String()},
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedCount:  3,
		},
		{
			name:           "error when peerTenantId is invalid",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"peerTenantId": "not-a-uuid"},
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when peerTenantId does not exist",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"peerTenantId": uuid.New().String()},
			user:           tnu1,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "tenant admin 1 filters by peerTenantId",
			reqOrgName:     tnOrg1,
			queryParams:    map[string]string{"peerTenantId": tn2.ID.String()},
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedCount:  4,
		},
		{
			name:                 "provider and tenant admin filters by peerTenantId for tenant 2",
			reqOrgName:           ipOrg2,
			queryParams:          map[string]string{"peerTenantId": tn2.ID.String()},
			user:                 ipu2,
			expectedStatus:       http.StatusOK,
			expectedCount:        1,
			requirePeerTenantIDs: []uuid.UUID{tn2.ID},
		},
		{
			name:                 "provider and tenant admin filters by peerTenantId for provider tenant",
			reqOrgName:           ipOrg2,
			queryParams:          map[string]string{"peerTenantId": tnProvider.ID.String()},
			user:                 ipu2,
			expectedStatus:       http.StatusOK,
			expectedCount:        1,
			requirePeerTenantIDs: []uuid.UUID{tnProvider.ID},
		},
		{
			name:                 "provider and tenant admin filters by multiple peerTenantId values",
			reqOrgName:           ipOrg2,
			queryString:          fmt.Sprintf("peerTenantId=%s&peerTenantId=%s", tn2.ID, tnProvider.ID),
			user:                 ipu2,
			expectedStatus:       http.StatusOK,
			expectedCount:        2,
			requirePeerTenantIDs: []uuid.UUID{tn2.ID, tnProvider.ID},
		},
		{
			name:              "tenant admin 1 filters by vpcId and peerTenantId",
			reqOrgName:        tnOrg1,
			queryParams:       map[string]string{"vpcId": vpc1.ID.String(), "peerTenantId": tn2.ID.String()},
			user:              tnu1,
			expectedStatus:    http.StatusOK,
			expectedCount:     1,
			validateTenantIDs: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gavph := GetAllVpcPeeringHandler{
				dbSession: dbSession,
				tc:        mockTC,
				cfg:       cfg,
			}

			e := echo.New()
			url := "/?"
			if tt.queryString != "" {
				url += tt.queryString
			} else {
				first := true
				for k, v := range tt.queryParams {
					if !first {
						url += "&"
					}
					url += fmt.Sprintf("%s=%s", k, v)
					first = false
				}
			}
			req := httptest.NewRequest(http.MethodGet, url, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrgName)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := gavph.Handle(ec)
			require.NoError(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if tt.expectedStatus == http.StatusOK {
				var list []model.APIVpcPeering
				err := json.Unmarshal(rec.Body.Bytes(), &list)
				require.NoError(t, err)
				assert.Equal(t, tt.expectedCount, len(list))
				if tt.validatePagination {
					paginationHeader := rec.Header().Get(pagination.ResponseHeaderName)
					assert.NotEmpty(t, paginationHeader)
					var pageResp pagination.PageResponse
					err := json.Unmarshal([]byte(paginationHeader), &pageResp)
					require.NoError(t, err)
					assert.Equal(t, tt.expectedCount, pageResp.Total)
				}
				if tt.validateTenantIDsPresent {
					for _, peering := range list {
						require.NotNil(t, peering.Vpc1.Tenant.ID)
						require.NotNil(t, peering.Vpc2.Tenant.ID)
					}
				}
				if tt.validateTenantIDs {
					require.NotEmpty(t, list)
					for _, peering := range list {
						require.NotNil(t, peering.Vpc1.Tenant.ID)
						require.NotNil(t, peering.Vpc2.Tenant.ID)
						tenantIDs := []string{peering.Vpc1.TenantID, peering.Vpc2.TenantID}
						assert.Contains(t, tenantIDs, tn1.ID.String())
						assert.Contains(t, tenantIDs, tn2.ID.String())
					}
				}
				if len(tt.requirePeerTenantIDs) > 0 {
					matchedPeerTenantIDs := make(map[uuid.UUID]bool, len(tt.requirePeerTenantIDs))
					for _, peering := range list {
						require.NotNil(t, peering.Vpc1.Tenant.ID)
						require.NotNil(t, peering.Vpc2.Tenant.ID)
						for _, tenantID := range tt.requirePeerTenantIDs {
							tenantIDStr := tenantID.String()
							if peering.Vpc1.Tenant.ID == tenantIDStr || peering.Vpc2.Tenant.ID == tenantIDStr {
								matchedPeerTenantIDs[tenantID] = true
							}
						}
					}
					for _, tenantID := range tt.requirePeerTenantIDs {
						assert.True(t, matchedPeerTenantIDs[tenantID], "expected at least one peering involving tenant %s", tenantID)
					}
				}
			}
		})
	}
}

func TestGetVpcPeeringHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	ipOrg2 := "test-provider-org-2"
	ipOrgRoles2 := []string{authz.ProviderAdminRole, authz.TenantAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	// Provider users
	// ip is a provider admin, ip2 is a provider tenant admin
	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider-1", ipOrg, ipu)
	ipu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg2, ipOrgRoles2)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider-2", ipOrg2, ipu2)

	// Sites
	st1 := common.TestBuildSite(t, dbSession, ip, "site-1", ipu)
	st2 := common.TestBuildSite(t, dbSession, ip, "site-2", ipu2)
	st3 := common.TestBuildSite(t, dbSession, ip2, "site-3", ipu2)
	for _, st := range []*cdbm.Site{st1, st2, st3} {
		st.Status = cdbm.SiteStatusRegistered
		_, err := dbSession.DB.NewUpdate().Model(st).Where("id = ?", st.ID).Exec(context.Background())
		assert.Nil(t, err)
	}

	// Tenants/users
	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "tenant-1", tnOrg1, tnu1)
	common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	common.TestBuildTenantSite(t, dbSession, tn1, st2, tnu1)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "tenant-2", tnOrg2, tnu2)
	common.TestBuildTenantSite(t, dbSession, tn2, st1, tnu2)
	common.TestBuildTenantSite(t, dbSession, tn2, st2, tnu2)
	common.TestBuildTenantSite(t, dbSession, tn2, st3, tnu2)

	// Tenant for provider+tenant admin user in provider org.
	tnProvider := common.TestBuildTenant(t, dbSession, "tenant-provider", ipOrg2, ipu2)
	common.TestBuildTenantSite(t, dbSession, tnProvider, st1, ipu2)

	// VPCs
	vpc1 := common.TestBuildVPC(t, dbSession, "vpc-1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc2 := common.TestBuildVPC(t, dbSession, "vpc-2", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc3 := common.TestBuildVPC(t, dbSession, "vpc-3", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc4 := common.TestBuildVPC(t, dbSession, "vpc-4", ip, tn2, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc5 := common.TestBuildVPC(t, dbSession, "vpc-5", ip, tn1, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc6 := common.TestBuildVPC(t, dbSession, "vpc-6", ip, tn2, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc7 := common.TestBuildVPC(t, dbSession, "vpc-7", ip, tnProvider, st1, nil, nil, nil, cdbm.VpcStatusReady, ipu2)
	vpc8 := common.TestBuildVPC(t, dbSession, "vpc-8", ip, tnProvider, st1, nil, nil, nil, cdbm.VpcStatusReady, ipu2)

	// Tenant-created peerings
	vp12 := common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc2.ID, st1.ID, nil, &tn1.ID, false, tnu1.ID)
	vp13 := common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc3.ID, st1.ID, nil, &tn1.ID, false, tnu1.ID)
	vp78 := common.TestBuildVpcPeering(t, dbSession, vpc7.ID, vpc8.ID, st1.ID, nil, &tnProvider.ID, false, ipu2.ID)

	// Provider-created peerings
	vp14 := common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc4.ID, st1.ID, &ip.ID, nil, true, ipu.ID)
	vp56 := common.TestBuildVpcPeering(t, dbSession, vpc5.ID, vpc6.ID, st2.ID, &ip2.ID, nil, true, ipu2.ID)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)
	mockTC := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name                   string
		reqOrgName             string
		peeringID              string
		user                   *cdbm.User
		includeRelations       []string
		expectedStatus         int
		expectedID             string
		validateTenantInfo     bool
		expectVpc1TenantID     string
		expectVpc1TenantOrg    string
		expectVpc2TenantID     string
		expectVpc2TenantOrg    string
		expectNoVpcSummaries   bool
		validatePeeringTenant  bool
		expectPeeringTenantID  string
		expectPeeringTenantOrg string
		expectNoPeeringTenant  bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			peeringID:      vp12.ID.String(),
			user:           nil,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user does not belong to org",
			reqOrgName:     "SomeOtherOrg",
			peeringID:      vp12.ID.String(),
			user:           tnu1,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg1,
			peeringID:      vp12.ID.String(),
			user:           tnu1Forbidden,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when peering ID is invalid",
			reqOrgName:     tnOrg1,
			peeringID:      "not-a-uuid",
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when VPC peering not found",
			reqOrgName:     tnOrg1,
			peeringID:      uuid.New().String(),
			user:           tnu1,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "tenant admin forbidden when neither VPC belongs to tenant",
			reqOrgName:     tnOrg2,
			peeringID:      vp12.ID.String(),
			user:           tnu2,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "tenant admin success when both VPCs belong to tenant",
			reqOrgName:     tnOrg1,
			peeringID:      vp12.ID.String(),
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedID:     vp12.ID.String(),
		},
		{
			name:           "tenant admin success when one VPC belongs to tenant",
			reqOrgName:     tnOrg1,
			peeringID:      vp14.ID.String(),
			user:           tnu1,
			expectedStatus: http.StatusOK,
			expectedID:     vp14.ID.String(),
		},
		{
			name:           "provider admin success for provider-created peering",
			reqOrgName:     ipOrg,
			peeringID:      vp14.ID.String(),
			user:           ipu,
			expectedStatus: http.StatusOK,
			expectedID:     vp14.ID.String(),
		},
		{
			name:           "provider admin forbidden for tenant-created peering",
			reqOrgName:     ipOrg,
			peeringID:      vp12.ID.String(),
			user:           ipu,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin forbidden for peering created by other provider",
			reqOrgName:     ipOrg,
			peeringID:      vp56.ID.String(),
			user:           ipu,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "both tenant admin and provider admin success through provider path",
			reqOrgName:     ipOrg2,
			peeringID:      vp56.ID.String(),
			user:           ipu2,
			expectedStatus: http.StatusOK,
			expectedID:     vp56.ID.String(),
		},
		{
			name:           "both tenant admin and provider admin success through tenant path",
			reqOrgName:     ipOrg2,
			peeringID:      vp78.ID.String(),
			user:           ipu2,
			expectedStatus: http.StatusOK,
			expectedID:     vp78.ID.String(),
		},
		{
			name:           "both tenant admin and provider admin forbidden when neither path matches",
			reqOrgName:     ipOrg2,
			peeringID:      vp13.ID.String(),
			user:           ipu2,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                "tenant info present when Vpc1 and Vpc2 included for same-tenant peering",
			reqOrgName:          tnOrg1,
			peeringID:           vp12.ID.String(),
			user:                tnu1,
			includeRelations:    []string{cdbm.Vpc1RelationName, cdbm.Vpc2RelationName},
			expectedStatus:      http.StatusOK,
			expectedID:          vp12.ID.String(),
			validateTenantInfo:  true,
			expectVpc1TenantID:  tn1.ID.String(),
			expectVpc1TenantOrg: tnOrg1,
			expectVpc2TenantID:  tn1.ID.String(),
			expectVpc2TenantOrg: tnOrg1,
		},
		{
			name:                "tenant info present when Vpc1 and Vpc2 included for cross-tenant peering",
			reqOrgName:          tnOrg1,
			peeringID:           vp14.ID.String(),
			user:                tnu1,
			includeRelations:    []string{cdbm.Vpc1RelationName, cdbm.Vpc2RelationName},
			expectedStatus:      http.StatusOK,
			expectedID:          vp14.ID.String(),
			validateTenantInfo:  true,
			expectVpc1TenantID:  tn1.ID.String(),
			expectVpc1TenantOrg: tnOrg1,
			expectVpc2TenantID:  tn2.ID.String(),
			expectVpc2TenantOrg: tnOrg2,
		},
		{
			name:                  "vpc summaries absent when includeRelation omitted",
			reqOrgName:            tnOrg1,
			peeringID:             vp12.ID.String(),
			user:                  tnu1,
			expectedStatus:        http.StatusOK,
			expectedID:            vp12.ID.String(),
			expectNoVpcSummaries:  true,
			expectNoPeeringTenant: true,
		},
		{
			name:                   "peering tenant summary present when Tenant included for tenant-created peering",
			reqOrgName:             tnOrg1,
			peeringID:              vp12.ID.String(),
			user:                   tnu1,
			includeRelations:       []string{cdbm.TenantRelationName},
			expectedStatus:         http.StatusOK,
			expectedID:             vp12.ID.String(),
			validatePeeringTenant:  true,
			expectPeeringTenantID:  tn1.ID.String(),
			expectPeeringTenantOrg: tnOrg1,
		},
		{
			name:                   "peering tenant summary present when Tenant included for provider tenant admin peering",
			reqOrgName:             ipOrg2,
			peeringID:              vp78.ID.String(),
			user:                   ipu2,
			includeRelations:       []string{cdbm.TenantRelationName},
			expectedStatus:         http.StatusOK,
			expectedID:             vp78.ID.String(),
			validatePeeringTenant:  true,
			expectPeeringTenantID:  tnProvider.ID.String(),
			expectPeeringTenantOrg: ipOrg2,
		},
		{
			name:                  "peering tenant summary absent for provider-created peering even when Tenant included",
			reqOrgName:            tnOrg1,
			peeringID:             vp14.ID.String(),
			user:                  tnu1,
			includeRelations:      []string{cdbm.TenantRelationName},
			expectedStatus:        http.StatusOK,
			expectedID:            vp14.ID.String(),
			expectNoPeeringTenant: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gvph := GetVpcPeeringHandler{
				dbSession: dbSession,
				tc:        mockTC,
				cfg:       cfg,
			}

			e := echo.New()
			url := "/"
			if len(tt.includeRelations) > 0 {
				for i, relation := range tt.includeRelations {
					if i == 0 {
						url += "?"
					} else {
						url += "&"
					}
					url += fmt.Sprintf("includeRelation=%s", relation)
				}
			}
			req := httptest.NewRequest(http.MethodGet, url, nil)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrgName, tt.peeringID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := gvph.Handle(ec)
			require.NoError(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if tt.expectedStatus == http.StatusOK {
				var apiVP model.APIVpcPeering
				err := json.Unmarshal(rec.Body.Bytes(), &apiVP)
				require.NoError(t, err)
				assert.Equal(t, tt.expectedID, apiVP.ID)
				if tt.validateTenantInfo {
					require.NotNil(t, apiVP.Vpc1)
					require.NotNil(t, apiVP.Vpc2)
					require.NotNil(t, apiVP.Vpc1.Tenant)
					require.NotNil(t, apiVP.Vpc2.Tenant)
					assert.Equal(t, tt.expectVpc1TenantID, apiVP.Vpc1.Tenant.ID)
					assert.Equal(t, tt.expectVpc1TenantOrg, apiVP.Vpc1.Tenant.Org)
					assert.Equal(t, tt.expectVpc2TenantID, apiVP.Vpc2.Tenant.ID)
					assert.Equal(t, tt.expectVpc2TenantOrg, apiVP.Vpc2.Tenant.Org)
				}
				if tt.expectNoVpcSummaries {
					assert.Nil(t, apiVP.Vpc1)
					assert.Nil(t, apiVP.Vpc2)
				}
				if tt.validatePeeringTenant {
					require.NotNil(t, apiVP.TenantID)
					require.NotNil(t, apiVP.Tenant)
					assert.Equal(t, tt.expectPeeringTenantID, *apiVP.TenantID)
					assert.Equal(t, tt.expectPeeringTenantOrg, apiVP.Tenant.Org)
				}
				if tt.expectNoPeeringTenant {
					assert.Nil(t, apiVP.Tenant)
					assert.Nil(t, apiVP.TenantID)
				}
			}
		})
	}
}

func TestDeleteVpcPeeringHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipOrgRoles := []string{authz.ProviderAdminRole}
	ipOrg2 := "test-provider-org-2"
	ipOrgRoles2 := []string{authz.ProviderAdminRole, authz.TenantAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	ipu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg2, ipOrgRoles2)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider-2", ipOrg2, ipu2)

	// Create sites and update them to Registered status
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)
	st1.Status = cdbm.SiteStatusRegistered
	_, err := dbSession.DB.NewUpdate().Model(st1).Where("id = ?", st1.ID).Exec(context.Background())
	assert.Nil(t, err)
	st2 := common.TestBuildSite(t, dbSession, ip2, "test-site-2", ipu2)
	st2.Status = cdbm.SiteStatusRegistered
	_, err = dbSession.DB.NewUpdate().Model(st2).Where("id = ?", st2.ID).Exec(context.Background())
	assert.Nil(t, err)

	// Build tenants for multi-tenant peering test
	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg1, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)
	t1s1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, t1s1)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)
	t2s1 := common.TestBuildTenantSite(t, dbSession, tn2, st1, tnu2)
	assert.NotNil(t, t2s1)
	t2s2 := common.TestBuildTenantSite(t, dbSession, tn2, st2, tnu2)
	assert.NotNil(t, t2s2)
	tnProvider := common.TestBuildTenant(t, dbSession, "test-tenant-provider", ipOrg2, ipu2)
	tProviders2 := common.TestBuildTenantSite(t, dbSession, tnProvider, st2, ipu2)
	assert.NotNil(t, tProviders2)

	// VPCs must be in Ready state for create peering to succeed
	vpc1 := common.TestBuildVPC(t, dbSession, "vpc-1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc2 := common.TestBuildVPC(t, dbSession, "vpc-2", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc3 := common.TestBuildVPC(t, dbSession, "vpc-3", ip, tn2, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc4 := common.TestBuildVPC(t, dbSession, "vpc-4", ip2, tn2, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc5 := common.TestBuildVPC(t, dbSession, "vpc-5", ip2, tn2, st2, nil, nil, nil, cdbm.VpcStatusReady, tnu2)
	vpc6 := common.TestBuildVPC(t, dbSession, "vpc-6", ip2, tnProvider, st2, nil, nil, nil, cdbm.VpcStatusReady, ipu2)
	vpc7 := common.TestBuildVPC(t, dbSession, "vpc-7", ip2, tnProvider, st2, nil, nil, nil, cdbm.VpcStatusReady, ipu2)

	// Create peerings between VPCs
	vp12 := common.TestBuildVpcPeering(t, dbSession, vpc1.ID, vpc2.ID, st1.ID, nil, &tn1.ID, false, tnu1.ID)
	vp23 := common.TestBuildVpcPeering(t, dbSession, vpc2.ID, vpc3.ID, st1.ID, &ip.ID, nil, true, ipu.ID)
	vp45 := common.TestBuildVpcPeering(t, dbSession, vpc4.ID, vpc5.ID, st2.ID, nil, &tn2.ID, false, tnu2.ID)
	vp67 := common.TestBuildVpcPeering(t, dbSession, vpc6.ID, vpc7.ID, st2.ID, nil, &tnProvider.ID, false, ipu2.ID)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	cfg := common.GetTestConfig()

	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("Get", mock.Anything, nil).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-delete-workflow-id")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteVpcPeering", mock.Anything).Return(mockWorkflowRun, nil)
	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
			st2.ID.String(): mockTC,
		},
	}

	tests := []struct {
		name           string
		reqOrgName     string
		peeringID      string
		user           *cdbm.User
		expectedStatus int
		expectDeleted  bool
		deletedID      uuid.UUID
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			peeringID:      vp12.ID.String(),
			user:           nil,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user does not belong to org",
			reqOrgName:     "SomeOtherOrg",
			peeringID:      vp12.ID.String(),
			user:           tnu1,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg1,
			peeringID:      vp12.ID.String(),
			user:           tnu1Forbidden,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when peering ID is invalid",
			reqOrgName:     tnOrg1,
			peeringID:      "invalid-uuid",
			user:           tnu1,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when VPC peering not found",
			reqOrgName:     tnOrg1,
			peeringID:      uuid.New().String(),
			user:           tnu1,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "Tenant Admin forbidden when only one VPC belongs to tenant",
			reqOrgName:     tnOrg1,
			peeringID:      vp23.ID.String(),
			user:           tnu1,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "Provider Admin forbidden when peering is single-tenant",
			reqOrgName:     ipOrg,
			peeringID:      vp12.ID.String(),
			user:           ipu,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "Provider Admin forbidden when peering is in another provider's site",
			reqOrgName:     ipOrg,
			peeringID:      vp45.ID.String(),
			user:           ipu,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "Tenant Admin success when both VPCs belong to tenant",
			reqOrgName:     tnOrg1,
			peeringID:      vp12.ID.String(),
			user:           tnu1,
			expectedStatus: http.StatusNoContent,
			expectDeleted:  true,
			deletedID:      vp12.ID,
		},
		{
			name:           "Provider Admin success when multi-tenant peering in their site",
			reqOrgName:     ipOrg,
			peeringID:      vp23.ID.String(),
			user:           ipu,
			expectedStatus: http.StatusNoContent,
			expectDeleted:  true,
			deletedID:      vp23.ID,
		},
		{
			name:           "user with both provider and tenant admin roles can delete single-tenant peering via tenant authorization path",
			reqOrgName:     ipOrg2,
			peeringID:      vp67.ID.String(),
			user:           ipu2,
			expectedStatus: http.StatusNoContent,
			expectDeleted:  true,
			deletedID:      vp67.ID,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dvph := DeleteVpcPeeringHandler{
				dbSession: dbSession,
				tc:        mockTC,
				scp:       mockSCP,
				cfg:       cfg,
			}

			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrgName, tt.peeringID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := dvph.Handle(ec)
			require.NoError(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if tt.expectDeleted && tt.deletedID != uuid.Nil {
				vpDAO := cdbm.NewVpcPeeringDAO(dbSession)
				_, err := vpDAO.GetByID(context.Background(), nil, tt.deletedID, nil)
				assert.True(t, errors.Is(err, cdb.ErrDoesNotExist), "expected peering to be deleted from DB")
			}
		})
	}
}
