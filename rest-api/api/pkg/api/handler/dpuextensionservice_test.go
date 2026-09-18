// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	sutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// TestCreateDpuExtensionServiceHandler_Handle tests the Create DPU Extension Service handler
func TestCreateDpuExtensionServiceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)
	// Update site to Registered status
	st1.Status = cdbm.SiteStatusRegistered
	_, err := dbSession.DB.NewUpdate().Model(st1).Where("id = ?", st1.ID).Exec(context.Background())
	assert.Nil(t, err)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	// Create existing DPU Extension Service to test name collision
	existingDES := common.TestBuildDpuExtensionService(t, dbSession, "existing-service", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, existingDES)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal client
	version := "V1-T1761856992374052"
	createdTime := time.Now().UTC().Round(time.Microsecond)
	obsName := "busybox-metrics"
	expectedObservability := &model.APIDpuExtensionServiceObservability{
		Configs: []model.APIDpuExtensionServiceObservabilityConfig{
			{
				Name: &obsName,
				Prometheus: &model.APIDpuExtensionServiceObservabilityConfigPrometheus{
					ScrapeIntervalSeconds: 30,
					Endpoint:              "busybox:9090",
				},
			},
		},
	}
	versionInfo := &corev1.DpuExtensionServiceVersionInfo{
		Version:       version,
		Data:          "apiVersion: v1\nkind: Pod",
		HasCredential: true,
		Created:       createdTime.Format(cdbm.DpuExtensionServiceTimeFormat),
		Observability: &corev1.DpuExtensionServiceObservability{
			Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
				{
					Name: &obsName,
					Config: &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
						Prometheus: &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
							ScrapeIntervalSeconds: 30,
							Endpoint:              "busybox:9090",
						},
					},
				},
			},
		},
	}
	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	var capturedCreateRequest *corev1.CreateDpuExtensionServiceRequest
	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		arg := args.Get(1)
		if ptr, ok := arg.(**corev1.DpuExtensionService); ok {
			*ptr = &corev1.DpuExtensionService{
				LatestVersionInfo: versionInfo,
				ActiveVersions:    []string{version},
			}
		}
	}).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateDpuExtensionService", mock.Anything).Run(func(args mock.Arguments) {
		capturedCreateRequest = args.Get(3).(*corev1.CreateDpuExtensionServiceRequest)
	}).Return(mockWorkflowRun, nil)

	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
		},
	}

	okBody := model.APIDpuExtensionServiceCreateRequest{
		Name:        "test-service",
		Description: cutil.GetPtr("Test Description"),
		ServiceType: model.DpuExtensionServiceTypeKubernetesPod,
		SiteID:      st1.ID.String(),
		Data:        "apiVersion: v1\nkind: Pod",
		Credentials: &model.APIDpuExtensionServiceCredentials{
			RegistryURL: "https://registry.example.com",
			Username:    cutil.GetPtr("testuser"),
			Password:    cutil.GetPtr("testpass"),
		},
		Observability: expectedObservability,
	}
	okBodyBytes, _ := json.Marshal(okBody)

	nameClashBody := okBody
	nameClashBody.Name = "existing-service"
	nameClashBodyBytes, _ := json.Marshal(nameClashBody)

	invalidSiteBody := okBody
	invalidSiteBody.SiteID = uuid.New().String()
	invalidSiteBodyBytes, _ := json.Marshal(invalidSiteBody)

	invalidBodyBytes := []byte(`{"name": "test"}`)

	tests := []struct {
		name           string
		reqOrgName     string
		reqBody        string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			reqBody:        string(okBodyBytes),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user does not belong to org",
			reqOrgName:     "SomeOtherOrg",
			reqBody:        string(okBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			reqBody:        string(okBodyBytes),
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body is invalid",
			reqOrgName:     tnOrg,
			reqBody:        string(invalidBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site does not exist",
			reqOrgName:     tnOrg,
			reqBody:        string(invalidSiteBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when name already exists",
			reqOrgName:     tnOrg,
			reqBody:        string(nameClashBodyBytes),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "success creating DPU Extension Service",
			reqOrgName:     tnOrg,
			reqBody:        string(okBodyBytes),
			user:           tnu1,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cdesh := CreateDpuExtensionServiceHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				scp:        mockSCP,
				cfg:        common.GetTestConfig(),
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
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

			err := cdesh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && rec.Code == http.StatusCreated {
				var apiDES model.APIDpuExtensionService
				err := json.Unmarshal(rec.Body.Bytes(), &apiDES)
				require.NoError(t, err)
				assert.Equal(t, okBody.Name, apiDES.Name)
				assert.Equal(t, okBody.ServiceType, apiDES.ServiceType)
				assert.Equal(t, okBody.SiteID, apiDES.SiteID)
				assert.Equal(t, version, *apiDES.Version)
				assert.Equal(t, []string{version}, apiDES.ActiveVersions)
				assert.Equal(t, createdTime, apiDES.VersionInfo.Created)
				require.NotNil(t, capturedCreateRequest)
				assert.Equal(t, okBody.Name, capturedCreateRequest.ServiceName)
				assert.Equal(t, okBody.Data, capturedCreateRequest.Data)
				require.NotNil(t, capturedCreateRequest.Credential)
				assert.Equal(t, okBody.Credentials.RegistryURL, capturedCreateRequest.Credential.RegistryUrl)
				require.NotNil(t, capturedCreateRequest.Observability)
				require.Greater(t, len(capturedCreateRequest.Observability.Configs), 0, "expected at least one captured observability config")
				capturedCreatePrometheus := capturedCreateRequest.Observability.Configs[0].GetPrometheus()
				require.NotNil(t, capturedCreatePrometheus, "expected first captured observability config to be prometheus")
				assert.Equal(t, expectedObservability.Configs[0].Prometheus.Endpoint, capturedCreatePrometheus.Endpoint)
				require.NotNil(t, apiDES.VersionInfo.Observability)
				require.Greater(t, len(apiDES.VersionInfo.Observability.Configs), 0, "expected at least one API observability config")
				require.NotNil(t, apiDES.VersionInfo.Observability.Configs[0].Prometheus, "expected first API observability config to be prometheus")
				assert.Equal(t, expectedObservability.Configs[0].Prometheus.Endpoint, apiDES.VersionInfo.Observability.Configs[0].Prometheus.Endpoint)
			}
		})
	}
}

// TestGetAllDpuExtensionServiceHandler_Handle tests the GetAll DPU Extension Service handler
func TestGetAllDpuExtensionServiceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)
	st1.Status = cdbm.SiteStatusRegistered
	_, err := dbSession.DB.NewUpdate().Model(st1).Where("id = ?", st1.ID).Exec(context.Background())
	assert.Nil(t, err)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	// Create multiple DPU Extension Services
	des1 := common.TestBuildDpuExtensionService(t, dbSession, "service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)
	des2 := common.TestBuildDpuExtensionService(t, dbSession, "service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusPending, tnu1)
	assert.NotNil(t, des2)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	mockTC := &tmocks.Client{}

	tests := []struct {
		name               string
		reqOrgName         string
		queryParams        map[string]string
		user               *cdbm.User
		expectedErr        bool
		expectedStatus     int
		expectedCount      int
		validatePagination bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user does not belong to org",
			reqOrgName:     "SomeOtherOrg",
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "success getting all DPU Extension Services",
			reqOrgName:     tnOrg,
			user:           tnu1,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCount:  2,
		},
		{
			name:       "success filtering by site",
			reqOrgName: tnOrg,
			queryParams: map[string]string{
				"siteId": st1.ID.String(),
			},
			user:           tnu1,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCount:  2,
		},
		{
			name:       "success filtering by status",
			reqOrgName: tnOrg,
			queryParams: map[string]string{
				"status": cdbm.DpuExtensionServiceStatusReady,
			},
			user:           tnu1,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCount:  1,
		},
		{
			name:       "success with pagination",
			reqOrgName: tnOrg,
			queryParams: map[string]string{
				"pageNumber": "1",
				"pageSize":   "1",
			},
			user:               tnu1,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCount:      1,
			validatePagination: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gadesh := GetAllDpuExtensionServiceHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
			e := echo.New()
			url := "/"
			if len(tt.queryParams) > 0 {
				url += "?"
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

			err := gadesh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && rec.Code == http.StatusOK {
				var apiDESList []model.APIDpuExtensionService
				err := json.Unmarshal(rec.Body.Bytes(), &apiDESList)
				require.NoError(t, err)
				assert.Equal(t, tt.expectedCount, len(apiDESList))

				if tt.validatePagination {
					paginationHeader := rec.Header().Get(pagination.ResponseHeaderName)
					assert.NotEmpty(t, paginationHeader)
					var pageResp pagination.PageResponse
					err := json.Unmarshal([]byte(paginationHeader), &pageResp)
					require.NoError(t, err)
					assert.Equal(t, 2, pageResp.Total)
				}
			}
		})
	}
}

// TestGetDpuExtensionServiceHandler_Handle tests the Get DPU Extension Service handler
func TestGetDpuExtensionServiceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)

	// Create DPU Extension Services
	des1 := common.TestBuildDpuExtensionService(t, dbSession, "service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)
	des2 := common.TestBuildDpuExtensionService(t, dbSession, "service-2", model.DpuExtensionServiceTypeKubernetesPod, tn2, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu2)
	assert.NotNil(t, des2)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	mockTC := &tmocks.Client{}

	tests := []struct {
		name                  string
		reqOrgName            string
		dpuExtensionServiceID string
		user                  *cdbm.User
		expectedErr           bool
		expectedStatus        int
		expectedDESName       string
		expectNotFoundError   bool
		expectForbiddenError  bool
	}{
		{
			name:                  "error when user not found in request context",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			user:                  nil,
			expectedErr:           true,
			expectedStatus:        http.StatusInternalServerError,
		},
		{
			name:                  "error when user does not belong to org",
			reqOrgName:            "SomeOtherOrg",
			dpuExtensionServiceID: des1.ID.String(),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when user role is forbidden",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			user:                  tnu1Forbidden,
			expectedErr:           true,
			expectedStatus:        http.StatusForbidden,
		},
		{
			name:                  "error when DPU Extension Service ID is invalid",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: "invalid-uuid",
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when DPU Extension Service does not exist",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: uuid.New().String(),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusNotFound,
			expectNotFoundError:   true,
		},
		{
			name:                  "error when DPU Extension Service does not belong to tenant",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des2.ID.String(),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusForbidden,
			expectForbiddenError:  true,
		},
		{
			name:                  "success getting DPU Extension Service",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusOK,
			expectedDESName:       "service-1",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gdesh := GetDpuExtensionServiceHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrgName, tt.dpuExtensionServiceID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := gdesh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && rec.Code == http.StatusOK {
				var apiDES model.APIDpuExtensionService
				err := json.Unmarshal(rec.Body.Bytes(), &apiDES)
				require.NoError(t, err)
				assert.Equal(t, tt.expectedDESName, apiDES.Name)
				assert.Equal(t, tt.dpuExtensionServiceID, apiDES.ID)
			}
		})
	}
}

// TestUpdateDpuExtensionServiceHandler_Handle tests the Update DPU Extension Service handler
func TestUpdateDpuExtensionServiceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)
	st1.Status = cdbm.SiteStatusRegistered
	_, err := dbSession.DB.NewUpdate().Model(st1).Where("id = ?", st1.ID).Exec(context.Background())
	assert.Nil(t, err)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	des1 := common.TestBuildDpuExtensionService(t, dbSession, "service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)
	des2 := common.TestBuildDpuExtensionService(t, dbSession, "service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des2)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal client
	version := "V1-T1761856992374065"
	createdTime := time.Now().UTC().Round(time.Microsecond)
	updateObsName := "service-logs"
	versionInfo := &corev1.DpuExtensionServiceVersionInfo{
		Version:       version,
		Data:          "apiVersion: v1\nkind: Pod",
		HasCredential: true,
		Created:       createdTime.Format(cdbm.DpuExtensionServiceTimeFormat),
		Observability: &corev1.DpuExtensionServiceObservability{
			Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
				{
					Name: &updateObsName,
					Config: &corev1.DpuExtensionServiceObservabilityConfig_Logging{
						Logging: &corev1.DpuExtensionServiceObservabilityConfigLogging{
							Path: "/var/log/service.log",
						},
					},
				},
			},
		},
	}

	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	var capturedUpdateRequest *corev1.UpdateDpuExtensionServiceRequest
	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		arg := args.Get(1)
		if ptr, ok := arg.(**corev1.DpuExtensionService); ok {
			*ptr = &corev1.DpuExtensionService{
				LatestVersionInfo: versionInfo,
				ActiveVersions:    []string{version},
			}
		}
	}).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "UpdateDpuExtensionService", mock.Anything).Run(func(args mock.Arguments) {
		capturedUpdateRequest = args.Get(3).(*corev1.UpdateDpuExtensionServiceRequest)
	}).Return(mockWorkflowRun, nil)

	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
		},
	}

	okBody := model.APIDpuExtensionServiceUpdateRequest{
		Name:        cutil.GetPtr("updated-service-name"),
		Description: cutil.GetPtr("Updated Description"),
	}
	okBodyBytes, _ := json.Marshal(okBody)

	nameClashBody := model.APIDpuExtensionServiceUpdateRequest{
		Name: cutil.GetPtr("service-2"),
	}
	nameClashBodyBytes, _ := json.Marshal(nameClashBody)

	invalidBodyBytes := []byte(`{"name": ""}`)

	okBody2 := model.APIDpuExtensionServiceUpdateRequest{
		Name:        cutil.GetPtr("updated-service-name-2"),
		Description: cutil.GetPtr("Updated Description"),
		Data:        cutil.GetPtr("apiVersion: v1\nkind: Pod\nmetadata:\n  name: updated-service"),
		Credentials: &model.APIDpuExtensionServiceCredentials{
			RegistryURL: "https://registry.hub.docker.com",
			Username:    cutil.GetPtr("testuser"),
			Password:    cutil.GetPtr("testpass"),
		},
		Observability: &model.APIDpuExtensionServiceObservability{
			Configs: []model.APIDpuExtensionServiceObservabilityConfig{
				{
					Name: &updateObsName,
					Logging: &model.APIDpuExtensionServiceObservabilityConfigLogging{
						Path: "/var/log/service.log",
					},
				},
			},
		},
	}
	okBody2Bytes, _ := json.Marshal(okBody2)

	tests := []struct {
		name                  string
		reqOrgName            string
		dpuExtensionServiceID string
		reqBody               string
		user                  *cdbm.User
		expectedErr           bool
		expectedStatus        int
	}{
		{
			name:                  "error when user not found in request context",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(okBodyBytes),
			user:                  nil,
			expectedErr:           true,
			expectedStatus:        http.StatusInternalServerError,
		},
		{
			name:                  "error when user does not belong to org",
			reqOrgName:            "SomeOtherOrg",
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(okBodyBytes),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when user role is forbidden",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(okBodyBytes),
			user:                  tnu1Forbidden,
			expectedErr:           true,
			expectedStatus:        http.StatusForbidden,
		},
		{
			name:                  "error when request body is invalid",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(invalidBodyBytes),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when DPU Extension Service does not exist",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: uuid.New().String(),
			reqBody:               string(okBodyBytes),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusNotFound,
		},
		{
			name:                  "error when name already exists",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(nameClashBodyBytes),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "success updating DPU Extension Service name/description",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(okBodyBytes),
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusOK,
		},
		{
			name:                  "success updating DPU Extension Service data/credentials",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			reqBody:               string(okBody2Bytes),
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusOK,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dpuExtensionServiceID := tt.dpuExtensionServiceID
			if strings.Contains(tt.name, "success updating DPU Extension Service") {
				freshService := common.TestBuildDpuExtensionService(
					t,
					dbSession,
					"service-"+uuid.NewString(),
					model.DpuExtensionServiceTypeKubernetesPod,
					tn1,
					st1,
					"V1-T1761856992374052",
					cdbm.DpuExtensionServiceStatusReady,
					tnu1,
				)
				require.NotNil(t, freshService)
				dpuExtensionServiceID = freshService.ID.String()
			}

			capturedUpdateRequest = nil

			udesh := UpdateDpuExtensionServiceHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				scp:        mockSCP,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(tt.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrgName, dpuExtensionServiceID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := udesh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && rec.Code == http.StatusOK {
				var apiDES model.APIDpuExtensionService
				err := json.Unmarshal(rec.Body.Bytes(), &apiDES)
				require.NoError(t, err)
				if strings.Contains(tt.name, "data/credentials") {
					assert.Equal(t, *okBody2.Name, apiDES.Name)
					assert.Equal(t, *okBody2.Description, *apiDES.Description)
					require.NotNil(t, capturedUpdateRequest)
					require.NotNil(t, capturedUpdateRequest.ServiceName)
					assert.Equal(t, *okBody2.Name, *capturedUpdateRequest.ServiceName)
					assert.Equal(t, *okBody2.Data, capturedUpdateRequest.Data)
					require.NotNil(t, capturedUpdateRequest.Observability)
					require.Greater(t, len(capturedUpdateRequest.Observability.Configs), 0, "expected at least one captured update observability config")
					require.NotNil(t, capturedUpdateRequest.Observability.Configs[0].Name, "expected first captured update observability config to have a name")
					assert.Equal(t, *okBody2.Observability.Configs[0].Name, *capturedUpdateRequest.Observability.Configs[0].Name)
					capturedUpdateLogging := capturedUpdateRequest.Observability.Configs[0].GetLogging()
					require.NotNil(t, capturedUpdateLogging, "expected first captured update observability config to be logging")
					assert.Equal(t, okBody2.Observability.Configs[0].Logging.Path, capturedUpdateLogging.Path)
				} else {
					assert.Equal(t, *okBody.Name, apiDES.Name)
					assert.Equal(t, *okBody.Description, *apiDES.Description)
					require.NotNil(t, capturedUpdateRequest)
					assert.NotNil(t, capturedUpdateRequest.ServiceName)
					assert.Equal(t, *okBody.Name, *capturedUpdateRequest.ServiceName)
				}
				assert.Equal(t, version, *apiDES.Version)
				assert.Equal(t, createdTime, apiDES.VersionInfo.Created)
				assert.Equal(t, []string{version}, apiDES.ActiveVersions)
				require.NotNil(t, apiDES.VersionInfo.Observability)
				require.Greater(t, len(apiDES.VersionInfo.Observability.Configs), 0, "expected at least one API observability config")
				require.NotNil(t, apiDES.VersionInfo.Observability.Configs[0].Logging, "expected first API observability config to be logging")
				assert.Equal(t, "/var/log/service.log", apiDES.VersionInfo.Observability.Configs[0].Logging.Path)
			}
		})
	}
}

// TestDeleteDpuExtensionServiceHandler_Handle tests the Delete DPU Extension Service handler
func TestDeleteDpuExtensionServiceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	des1 := common.TestBuildDpuExtensionService(t, dbSession, "service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)
	des2 := common.TestBuildDpuExtensionService(t, dbSession, "service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des2)

	al1 := common.TestBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	it1 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-1", nil, st1, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, tnu1)
	common.TestBuildAllocationConstraint(t, dbSession, al1, it1, nil, 5, tnu1)
	m1 := common.TestBuildMachine(t, dbSession, ip, st1, &it1.ID, nil, cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, m1, it1)
	vpc1 := common.TestBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	os1 := common.TestBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemStatusReady, tnu1)
	i1 := common.TestBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, it1.ID, vpc1.ID, &m1.ID, os1.ID)
	assert.NotNil(t, i1)

	// Create a deployment for des2 to test active deployment check
	desd := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des2, i1.ID, "v1", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal client
	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteDpuExtensionService", mock.Anything).Return(mockWorkflowRun, nil)

	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
		},
	}

	tests := []struct {
		name                  string
		reqOrgName            string
		dpuExtensionServiceID string
		user                  *cdbm.User
		expectedErr           bool
		expectedStatus        int
	}{
		{
			name:                  "error when user not found in request context",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			user:                  nil,
			expectedErr:           true,
			expectedStatus:        http.StatusInternalServerError,
		},
		{
			name:                  "error when user does not belong to org",
			reqOrgName:            "SomeOtherOrg",
			dpuExtensionServiceID: des1.ID.String(),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when user role is forbidden",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			user:                  tnu1Forbidden,
			expectedErr:           true,
			expectedStatus:        http.StatusForbidden,
		},
		{
			name:                  "error when DPU Extension Service does not exist",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: uuid.New().String(),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusNotFound,
		},
		{
			name:                  "error when DPU Extension Service has active deployments",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des2.ID.String(),
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "success deleting DPU Extension Service",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusNoContent,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ddesh := DeleteDpuExtensionServiceHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				scp:        mockSCP,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrgName, tt.dpuExtensionServiceID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := ddesh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && rec.Code == http.StatusNoContent {
				// Verify the service status is updated to Deleting
				desDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)
				_, err = desDAO.GetByID(context.Background(), nil, uuid.MustParse(tt.dpuExtensionServiceID), nil)
				require.Error(t, err)
				assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
			}
		})
	}
}

// TestGetDpuExtensionServiceVersionHandler_Handle tests the Get DPU Extension Service Version handler
func TestGetDpuExtensionServiceVersionHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	des1 := common.TestBuildDpuExtensionService(t, dbSession, "service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal client
	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}

	// Mock version info response
	version := "V1-T1761856992374052"
	createdTime := time.Now().UTC().Round(time.Microsecond)
	versionObsName := "version-metrics"
	mockVersionInfo := &corev1.DpuExtensionServiceVersionInfoList{
		VersionInfos: []*corev1.DpuExtensionServiceVersionInfo{
			{
				Version:       version,
				Data:          "apiVersion: v1\nkind: Pod",
				HasCredential: true,
				Created:       createdTime.Format(cdbm.DpuExtensionServiceTimeFormat),
				Observability: &corev1.DpuExtensionServiceObservability{
					Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
						{
							Name: &versionObsName,
							Config: &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
								Prometheus: &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
									ScrapeIntervalSeconds: 60,
									Endpoint:              "service:9090",
								},
							},
						},
					},
				},
			},
		},
	}

	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		arg := args.Get(1)
		if ptr, ok := arg.(**corev1.DpuExtensionServiceVersionInfoList); ok {
			*ptr = mockVersionInfo
		}
	}).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "GetDpuExtensionServiceVersionsInfo", mock.Anything).Return(mockWorkflowRun, nil)

	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
		},
	}

	tests := []struct {
		name                  string
		reqOrgName            string
		dpuExtensionServiceID string
		versionID             string
		user                  *cdbm.User
		expectedErr           bool
		expectedStatus        int
	}{
		{
			name:                  "error when user not found in request context",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			versionID:             "v1",
			user:                  nil,
			expectedErr:           true,
			expectedStatus:        http.StatusInternalServerError,
		},
		{
			name:                  "error when user does not belong to org",
			reqOrgName:            "SomeOtherOrg",
			dpuExtensionServiceID: des1.ID.String(),
			versionID:             "v1",
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when user role is forbidden",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			versionID:             "v1",
			user:                  tnu1Forbidden,
			expectedErr:           true,
			expectedStatus:        http.StatusForbidden,
		},
		{
			name:                  "error when DPU Extension Service does not exist",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: uuid.New().String(),
			versionID:             "v1",
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusNotFound,
		},
		{
			name:                  "success getting DPU Extension Service version",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID.String(),
			versionID:             "v1",
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusOK,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gdesvh := GetDpuExtensionServiceVersionHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				scp:        mockSCP,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id", "version")
			ec.SetParamValues(tt.reqOrgName, tt.dpuExtensionServiceID, tt.versionID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := gdesvh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if !tt.expectedErr && rec.Code == http.StatusOK {
				var apiVersionInfo model.APIDpuExtensionServiceVersionInfo
				err := json.Unmarshal(rec.Body.Bytes(), &apiVersionInfo)
				require.NoError(t, err)
				assert.NotNil(t, apiVersionInfo.Version)
				assert.Equal(t, version, apiVersionInfo.Version)
				assert.Equal(t, createdTime, apiVersionInfo.Created)
				require.NotNil(t, apiVersionInfo.Observability)
				require.Greater(t, len(apiVersionInfo.Observability.Configs), 0, "expected at least one API version observability config")
				require.NotNil(t, apiVersionInfo.Observability.Configs[0].Prometheus, "expected first API version observability config to be prometheus")
				assert.Equal(t, "service:9090", apiVersionInfo.Observability.Configs[0].Prometheus.Endpoint)
			}
		})
	}
}

// TestDeleteDpuExtensionServiceVersionHandler_Handle tests the Delete DPU Extension Service Version handler
func TestDeleteDpuExtensionServiceVersionHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := common.TestBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := common.TestBuildSite(t, dbSession, ip, "test-site-1", ipu)

	tnu1 := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu1Forbidden := common.TestBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	tn1 := common.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	ts1 := common.TestBuildTenantSite(t, dbSession, tn1, st1, tnu1)
	assert.NotNil(t, ts1)

	// Service with a single version, has no active deployment
	des1 := common.TestBuildDpuExtensionService(t, dbSession, "service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1770858775402975", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)

	// Service with two versions, has no active deployment
	des2 := common.TestBuildDpuExtensionService(t, dbSession, "service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1770859049413263", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des2)
	// Add a new version to the service
	des2OldVersion := *des2.Version

	des2.Version = cutil.GetPtr("V2-T1770859063836809")
	des2.VersionInfo = &cdbm.DpuExtensionServiceVersionInfo{
		Version:        *des2.Version,
		Data:           "apiVersion: v1\nkind: Pod",
		HasCredentials: true,
		Created:        time.Now().UTC().Round(time.Microsecond),
	}
	des2.ActiveVersions = []string{*des2.Version, des2OldVersion}
	_, err := dbSession.DB.NewUpdate().Model(des2).Where("id = ?", des2.ID).Exec(context.Background())
	assert.Nil(t, err)

	// Another service with two versions, has not active deployment
	des3 := common.TestBuildDpuExtensionService(t, dbSession, "service-3", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1770859074000664", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des3)
	// Add a new version to the service
	des3OldVersion := *des3.Version
	des3OldVersionInfo := *des3.VersionInfo

	des3.Version = cutil.GetPtr("V2-T1770859101840849")
	des3.VersionInfo = &cdbm.DpuExtensionServiceVersionInfo{
		Version:        *des3.Version,
		Data:           "apiVersion: v1\nkind: Pod",
		HasCredentials: true,
		Created:        time.Now().UTC().Round(time.Microsecond),
	}
	des3.ActiveVersions = []string{*des3.Version, des3OldVersion}
	_, err = dbSession.DB.NewUpdate().Model(des3).Where("id = ?", des3.ID).Exec(context.Background())
	assert.Nil(t, err)

	// Service with single version, has active deployment
	des4 := common.TestBuildDpuExtensionService(t, dbSession, "service-4", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1770859182030889", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des4)

	// Prep objects to attach deployment
	al1 := common.TestBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	it1 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-1", nil, st1, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, tnu1)
	common.TestBuildAllocationConstraint(t, dbSession, al1, it1, nil, 5, tnu1)
	m1 := common.TestBuildMachine(t, dbSession, ip, st1, &it1.ID, nil, cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, m1, it1)
	vpc1 := common.TestBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	os1 := common.TestBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemStatusReady, tnu1)
	i1 := common.TestBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, it1.ID, vpc1.ID, &m1.ID, os1.ID)
	assert.NotNil(t, i1)

	// Create a deployment for des2 to test active deployment check
	desd := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des4, i1.ID, *des4.Version, cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal client
	mockTC := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockWorkflowRun.On("GetID").Return("test-workflow-id-1")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteDpuExtensionService", mock.Anything).Return(mockWorkflowRun, nil)

	// Mock fetching older version info for des3 version
	mockVersionInfo := &corev1.DpuExtensionServiceVersionInfoList{
		VersionInfos: []*corev1.DpuExtensionServiceVersionInfo{
			{
				Version:       des3OldVersionInfo.Version,
				Data:          des3OldVersionInfo.Data,
				HasCredential: des3OldVersionInfo.HasCredentials,
				Created:       des3OldVersionInfo.Created.Format(cdbm.DpuExtensionServiceTimeFormat),
			},
		},
	}

	mockWorkflowRun2 := &tmocks.WorkflowRun{}
	mockWorkflowRun2.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		arg := args.Get(1)
		if ptr, ok := arg.(**corev1.DpuExtensionServiceVersionInfoList); ok {
			*ptr = mockVersionInfo
		}
	}).Return(nil)
	mockWorkflowRun2.On("GetID").Return("test-workflow-id-2")
	mockTC.On("ExecuteWorkflow", mock.Anything, mock.Anything, "GetDpuExtensionServiceVersionsInfo", mock.Anything).Return(mockWorkflowRun2, nil)

	// Mock Site Client Pool
	mockSCP := &sc.ClientPool{
		IDClientMap: map[string]temporalClient.Client{
			st1.ID.String(): mockTC,
		},
	}

	tests := []struct {
		name                  string
		reqOrgName            string
		dpuExtensionServiceID uuid.UUID
		versionID             string
		user                  *cdbm.User
		expectedErr           bool
		expectedStatus        int
		expectedVersion       *string
		expectServiceDeletion bool
	}{
		{
			name:                  "error when user not found in request context",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID,
			versionID:             *des1.Version,
			user:                  nil,
			expectedErr:           true,
			expectedStatus:        http.StatusInternalServerError,
		},
		{
			name:                  "error when user does not belong to org",
			reqOrgName:            "test-invalid-org",
			dpuExtensionServiceID: des1.ID,
			versionID:             *des1.Version,
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "error when user does not have Tenant Admin role",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID,
			versionID:             *des1.Version,
			user:                  tnu1Forbidden,
			expectedErr:           true,
			expectedStatus:        http.StatusForbidden,
		},
		{
			name:                  "error when DPU Extension Service does not exist",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: uuid.New(),
			versionID:             "V1-T1770859801962982",
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusNotFound,
		},
		{
			name:                  "error when version does not exist",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID,
			versionID:             "V2-T1770859822516497",
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusNotFound,
		},
		{
			name:                  "error when version has active deployments",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des4.ID,
			versionID:             *des4.Version,
			user:                  tnu1,
			expectedErr:           true,
			expectedStatus:        http.StatusBadRequest,
		},
		{
			name:                  "success deleting DPU Extension Service version",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des1.ID,
			versionID:             *des1.Version,
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusNoContent,
			expectServiceDeletion: true,
		},
		{
			name:                  "success deleting DPU Extension Service version when it is not the latest version",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des2.ID,
			versionID:             des2.ActiveVersions[1],
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusNoContent,
			expectedVersion:       cutil.GetPtr(des2.ActiveVersions[0]),
		},
		{
			name:                  "success deleting latest DPU Extension Service version",
			reqOrgName:            tnOrg,
			dpuExtensionServiceID: des3.ID,
			versionID:             *des3.Version,
			user:                  tnu1,
			expectedErr:           false,
			expectedStatus:        http.StatusNoContent,
			expectedVersion:       cutil.GetPtr(des3OldVersion),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ddesvh := DeleteDpuExtensionServiceVersionHandler{
				dbSession:  dbSession,
				tc:         mockTC,
				scp:        mockSCP,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id", "version")
			ec.SetParamValues(tt.reqOrgName, tt.dpuExtensionServiceID.String(), tt.versionID)
			ec.Set("user", tt.user)

			testCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(testCtx))

			err := ddesvh.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.expectedStatus, rec.Code)

			if rec.Code != http.StatusNoContent {
				return
			}

			desDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)

			// Verify service status
			des, err := desDAO.GetByID(context.Background(), nil, tt.dpuExtensionServiceID, nil)
			if tt.expectServiceDeletion {
				require.Error(t, err)
				assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
				return
			} else {
				require.NoError(t, err)
				assert.NotContains(t, des.ActiveVersions, tt.versionID)
			}

			if tt.expectedVersion != nil {
				// Verify that latest version matches expected version
				assert.Equal(t, *tt.expectedVersion, *des.Version)
				assert.Equal(t, *tt.expectedVersion, des.ActiveVersions[0])
				require.NotNil(t, des.VersionInfo)
				assert.Equal(t, *tt.expectedVersion, des.VersionInfo.Version)
			}
		})
	}
}
