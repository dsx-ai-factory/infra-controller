// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	tmocks "go.temporal.io/sdk/mocks"
)

// testExpectedPowerShelfInitDB initializes a test database session
func testExpectedPowerShelfInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))

	ctx := context.Background()

	err := dbSession.DB.ResetModel(ctx, (*cdbm.User)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.Site)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.ExpectedPowerShelf)(nil))
	assert.Nil(t, err)

	return dbSession
}

// testExpectedPowerShelfSetupTestData creates test infrastructure provider and site
func testExpectedPowerShelfSetupTestData(t *testing.T, dbSession *cdb.Session, org string) (*cdbm.InfrastructureProvider, *cdbm.Site) {
	ctx := context.Background()

	ip := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "test-provider",
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(ctx)
	assert.Nil(t, err)

	site := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site",
		Org:                      org,
		InfrastructureProviderID: ip.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(site).Exec(ctx)
	assert.Nil(t, err)

	return ip, site
}

func TestCreateExpectedPowerShelfHandler_Handle(t *testing.T) {
	e := echo.New()

	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site := testExpectedPowerShelfSetupTestData(t, dbSession, org)

	ctx := context.Background()
	unmanagedIP := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "unmanaged-provider",
		Org:  "other-org",
	}
	_, err := dbSession.DB.NewInsert().Model(unmanagedIP).Exec(ctx)
	assert.Nil(t, err)

	unmanagedSite := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "unmanaged-site",
		Org:                      "other-org",
		InfrastructureProviderID: unmanagedIP.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSite).Exec(ctx)
	assert.Nil(t, err)

	// Create an existing Expected Power Shelf with a specific MAC address for duplicate test
	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	existingMAC := "AA:BB:CC:DD:EE:11"
	_, err = epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        existingMAC,
		ShelfSerialNumber:    "EXISTING-SHELF-001",
		Labels:               map[string]string{"env": "existing"},
	})
	assert.Nil(t, err)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedPowerShelf", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewCreateExpectedPowerShelfHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
				},
			},
		}
	}

	validBmcIpAddress := "192.168.1.100"

	tests := []struct {
		name           string
		requestBody    model.APIExpectedPowerShelfCreateRequest
		setupContext   func(c echo.Context)
		expectedStatus int
	}{
		{
			name: "successful creation",
			requestBody: model.APIExpectedPowerShelfCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				ShelfSerialNumber:  "SHELF123",
				BmcIpAddress:       &validBmcIpAddress,
				Labels:             map[string]string{"env": "test"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusCreated,
		},
		{
			name: "missing user context",
			requestBody: model.APIExpectedPowerShelfCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:77",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				ShelfSerialNumber:  "SHELF125",
			},
			setupContext: func(c echo.Context) {
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name: "invalid mac address length",
			requestBody: model.APIExpectedPowerShelfCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				ShelfSerialNumber:  "SHELF126",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "site not found",
			requestBody: model.APIExpectedPowerShelfCreateRequest{
				SiteID:             "12345678-1234-1234-1234-123456789099",
				BmcMacAddress:      "00:11:22:33:44:88",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				ShelfSerialNumber:  "SHELF127",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "cannot create on unmanaged site",
			requestBody: model.APIExpectedPowerShelfCreateRequest{
				SiteID:             unmanagedSite.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:99",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				ShelfSerialNumber:  "SHELF128",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusForbidden,
		},
		{
			name: "duplicate MAC address should return 409",
			requestBody: model.APIExpectedPowerShelfCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      existingMAC,
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				ShelfSerialNumber:  "DUPLICATE-SHELF-999",
				Labels:             map[string]string{"env": "duplicate-test"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusConflict,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			reqBody, _ := json.Marshal(tt.requestBody)
			req := httptest.NewRequest(http.MethodPost, "/v2/org/test-org/nico/expected-power-shelf", bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			err := handler.Handle(c)

			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			if tt.expectedStatus == http.StatusCreated && tt.requestBody.Labels != nil {
				var response model.APIExpectedPowerShelf
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.NotNil(t, response.Labels, "Labels should not be nil in response")
				assert.Equal(t, tt.requestBody.Labels, map[string]string(response.Labels), "Labels in response should match request")
			}
		})
	}
}

func TestGetAllExpectedPowerShelfHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := &config.Config{}
	handler := NewGetAllExpectedPowerShelfHandler(dbSession, cfg)

	org := "test-org"
	infraProv, site := testExpectedPowerShelfSetupTestData(t, dbSession, org)

	unmanagedIP := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "unmanaged-provider",
		Org:  "other-org",
	}
	_, err := dbSession.DB.NewInsert().Model(unmanagedIP).Exec(ctx)
	assert.Nil(t, err)

	unmanagedSite := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "unmanaged-site",
		Org:                      "other-org",
		InfrastructureProviderID: unmanagedIP.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSite).Exec(ctx)
	assert.Nil(t, err)

	// Create expected power shelves - one on managed site, one on unmanaged site
	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	managedEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "00:11:22:33:44:AA",
		ShelfSerialNumber:    "MANAGED-SHELF",
		Labels:               map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, managedEPS)

	unmanagedEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               unmanagedSite.ID,
		BmcMacAddress:        "00:11:22:33:44:BB",
		ShelfSerialNumber:    "UNMANAGED-SHELF",
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedEPS)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderViewerRole},
				},
			},
		}
	}
	dualRoleTenant := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "dual-role-tenant",
		Org:  org,
	}
	_, err = dbSession.DB.NewInsert().Model(dualRoleTenant).Exec(ctx)
	assert.Nil(t, err)
	createDualRoleUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("dual-role-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          124,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderViewerRole, authz.TenantAdminRole},
				},
			},
		}
	}

	tests := []struct {
		name                 string
		siteId               string
		includeRelations     []string
		setupContext         func(c echo.Context)
		expectedStatus       int
		checkResponseContent func(t *testing.T, body []byte)
	}{
		{
			name:   "successful GetAll without siteId (lists only managed sites)",
			siteId: "",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APIExpectedPowerShelf
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, eps := range response {
					assert.NotEqual(t, unmanagedEPS.ID, eps.ID, "Unmanaged power shelf should not be in response")
				}
			},
		},
		{
			name:   "dual-role caller without siteId uses provider-wide listing",
			siteId: "",
			setupContext: func(c echo.Context) {
				c.Set("user", createDualRoleUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APIExpectedPowerShelf
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, eps := range response {
					assert.NotEqual(t, unmanagedEPS.ID, eps.ID, "Unmanaged power shelf should not be in response")
				}
			},
		},
		{
			name:   "successful GetAll with valid siteId",
			siteId: site.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APIExpectedPowerShelf
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, eps := range response {
					assert.Equal(t, site.ID, eps.SiteID, "All results should be from the specified site")
				}
			},
		},
		{
			name:             "successful GetAll with includeRelation=Site",
			siteId:           "",
			includeRelations: []string{"Site"},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APIExpectedPowerShelf
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Greater(t, len(response), 0, "Should return at least one expected power shelf")
				for _, eps := range response {
					assert.NotNil(t, eps.Site, "Site relation should be loaded")
					assert.Equal(t, eps.SiteID.String(), eps.Site.ID, "Site ID should match")
				}
			},
		},
		{
			name:   "cannot retrieve from unmanaged site",
			siteId: unmanagedSite.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/expected-power-shelf"
			params := []string{}
			if tt.siteId != "" {
				params = append(params, "siteId="+tt.siteId)
			}
			for _, relation := range tt.includeRelations {
				params = append(params, "includeRelation="+relation)
			}
			if len(params) > 0 {
				url += "?" + params[0]
				for i := 1; i < len(params); i++ {
					url += "&" + params[i]
				}
			}
			req := httptest.NewRequest(http.MethodGet, url, nil)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			err := handler.Handle(c)

			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			if tt.checkResponseContent != nil && rec.Code == http.StatusOK {
				tt.checkResponseContent(t, rec.Body.Bytes())
			}
		})
	}
}

func TestGetExpectedPowerShelfHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()

	cfg := &config.Config{}
	handler := NewGetExpectedPowerShelfHandler(dbSession, cfg)

	org := "test-org"
	infraProv, site := testExpectedPowerShelfSetupTestData(t, dbSession, org)

	unmanagedIP := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "unmanaged-provider",
		Org:  "other-org",
	}
	_, err := dbSession.DB.NewInsert().Model(unmanagedIP).Exec(ctx)
	assert.Nil(t, err)

	unmanagedSite := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "unmanaged-site",
		Org:                      "other-org",
		InfrastructureProviderID: unmanagedIP.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSite).Exec(ctx)
	assert.Nil(t, err)

	// Create a test ExpectedPowerShelf on managed site
	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	testEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "00:11:22:33:44:55",
		ShelfSerialNumber:    "TEST-SHELF-123",
		Labels:               map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, testEPS)

	// Create a test ExpectedPowerShelf on unmanaged site
	unmanagedEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               unmanagedSite.ID,
		BmcMacAddress:        "00:11:22:33:44:CC",
		ShelfSerialNumber:    "UNMANAGED-SHELF-456",
		Labels:               map[string]string{"env": "unmanaged"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedEPS)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
				},
			},
		}
	}

	tests := []struct {
		name           string
		id             string
		setupContext   func(c echo.Context)
		expectedStatus int
	}{
		{
			name: "invalid ID",
			id:   "invalid-id",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "invalid-id")
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "successful retrieval",
			id:   testEPS.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus: http.StatusOK,
		},
		{
			name: "power shelf not found",
			id:   "12345678-1234-1234-1234-123456789099",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "12345678-1234-1234-1234-123456789099")
			},
			expectedStatus: http.StatusNotFound,
		},
		{
			name: "cannot retrieve from unmanaged site",
			id:   unmanagedEPS.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedEPS.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/expected-power-shelf/" + tt.id
			req := httptest.NewRequest(http.MethodGet, url, nil)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			err := handler.Handle(c)

			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			if tt.expectedStatus == http.StatusOK && tt.name == "successful retrieval" {
				var response model.APIExpectedPowerShelf
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.NotNil(t, response.Labels, "Labels should not be nil in response")
				assert.Equal(t, "test", response.Labels["env"], "Labels in response should contain the 'env' label with value 'test'")
			}
		})
	}
}

func TestUpdateExpectedPowerShelfHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site := testExpectedPowerShelfSetupTestData(t, dbSession, org)

	unmanagedIP := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "unmanaged-provider",
		Org:  "other-org",
	}
	_, err := dbSession.DB.NewInsert().Model(unmanagedIP).Exec(ctx)
	assert.Nil(t, err)

	unmanagedSite := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "unmanaged-site",
		Org:                      "other-org",
		InfrastructureProviderID: unmanagedIP.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSite).Exec(ctx)
	assert.Nil(t, err)

	// Create a test ExpectedPowerShelf on managed site
	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	testEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "00:11:22:33:44:DD",
		ShelfSerialNumber:    "UPDATE-SHELF-123",
		Labels:               map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, testEPS)

	// Create a test ExpectedPowerShelf on unmanaged site
	unmanagedEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               unmanagedSite.ID,
		BmcMacAddress:        "00:11:22:33:44:EE",
		ShelfSerialNumber:    "UNMANAGED-UPDATE-456",
		Labels:               map[string]string{"env": "unmanaged"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedEPS)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	var capturedPatch *corev1.PatchExpectedPowerShelfRequest
	var capturedProxy grpcproxy.Request
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			capturedPatch = &corev1.PatchExpectedPowerShelfRequest{}
			testDecodeExpectedComponentPatch(t, args.Get(3), site.ID.String(), capturedPatch)
			capturedProxy = args.Get(3).(grpcproxy.Request)
			assert.Equal(t, corev1.Forge_PatchExpectedPowerShelf_FullMethodName, capturedProxy.FullMethod)
		}).
		Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewUpdateExpectedPowerShelfHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
				},
			},
		}
	}

	tests := []struct {
		name                 string
		id                   string
		requestBody          model.APIExpectedPowerShelfUpdateRequest
		setupContext         func(c echo.Context)
		expectedStatus       int
		expectedBmcMac       string
		expectedStoredBmcMac string
		expectedErrorMsg     string
		expectNoWorkflow     bool
		expectedPaths        []string
		rejectsCredentials   bool
	}{
		{
			name: "password-only update reaches Core through encrypted transport",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				DefaultBmcPassword: cutil.GetPtr("patch-secret"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"bmc_password"},
		},
		{
			name: "username-only update reaches Core through encrypted transport",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				DefaultBmcUsername: cutil.GetPtr("patch-admin"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"bmc_username"},
		},
		{
			name: "metadata update excludes null BMC credentials",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: cutil.GetPtr("UPDATED-SHELF-123"),
				Labels:            map[string]string{"env": "updated"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"metadata.labels", "shelf_serial_number"},
		},
		{
			name: "empty BMC username rejects accompanying metadata before dispatch",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				DefaultBmcUsername: cutil.GetPtr(""),
				Labels:             map[string]string{"env": "must-not-change"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus:     http.StatusBadRequest,
			expectedErrorMsg:   "defaultBmcUsername",
			expectNoWorkflow:   true,
			rejectsCredentials: true,
		},
		{
			name: "BMC MAC formatting difference preserves stored identity",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				BmcMacAddress: cutil.GetPtr("00-11-22-33-44-dd"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus:       http.StatusOK,
			expectedBmcMac:       testEPS.BmcMacAddress,
			expectedStoredBmcMac: testEPS.BmcMacAddress,
		},
		{
			name: "BMC MAC address cannot be changed",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				BmcMacAddress: cutil.GetPtr("AA:BB:CC:DD:EE:FF"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus:       http.StatusBadRequest,
			expectedStoredBmcMac: testEPS.BmcMacAddress,
			expectedErrorMsg:     "BMC MAC address cannot be changed after creation",
			expectNoWorkflow:     true,
		},
		{
			name: "body ID mismatch with URL should return 400",
			id:   testEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				ID:                cutil.GetPtr(uuid.New().String()),
				ShelfSerialNumber: cutil.GetPtr("SHOULD-NOT-UPDATE"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "cannot update on unmanaged site",
			id:   unmanagedEPS.ID.String(),
			requestBody: model.APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: cutil.GetPtr("SHOULD-NOT-UPDATE"),
				Labels:            map[string]string{"env": "fail"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedEPS.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			reqBody, _ := json.Marshal(tt.requestBody)
			url := "/v2/org/" + org + "/nico/expected-power-shelf/" + tt.id
			req := httptest.NewRequest(http.MethodPatch, url, bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			workflowCallsBefore := len(mockTemporalClient.Calls)
			var before *cdbm.ExpectedPowerShelf
			if tt.rejectsCredentials {
				before, err = epsDAO.Get(ctx, nil, testEPS.ID, nil, false)
				require.NoError(t, err)
			}
			err := handler.Handle(c)

			assert.Nil(t, err)
			if rec.Code == http.StatusOK {
				require.Len(t, mockTemporalClient.Calls, workflowCallsBefore+1)
				require.NotNil(t, capturedPatch)
				require.NotNil(t, capturedPatch.UpdateMask)
				assert.Equal(t, tt.expectedPaths, capturedPatch.UpdateMask.Paths)
				if tt.requestBody.DefaultBmcUsername != nil {
					assert.Equal(t, *tt.requestBody.DefaultBmcUsername, capturedPatch.ExpectedPowerShelf.BmcUsername)
					testExpectedComponentPatchSecrets(t, capturedProxy, *tt.requestBody.DefaultBmcUsername)
					assert.NotContains(t, rec.Body.String(), *tt.requestBody.DefaultBmcUsername)
				} else {
					assert.Empty(t, capturedPatch.ExpectedPowerShelf.BmcUsername)
				}
				if tt.requestBody.DefaultBmcPassword != nil {
					assert.Equal(t, *tt.requestBody.DefaultBmcPassword, capturedPatch.ExpectedPowerShelf.BmcPassword)
					testExpectedComponentPatchSecrets(t, capturedProxy, *tt.requestBody.DefaultBmcPassword)
					assert.NotContains(t, rec.Body.String(), *tt.requestBody.DefaultBmcPassword)
				} else {
					assert.Empty(t, capturedPatch.ExpectedPowerShelf.BmcPassword)
				}
				if tt.requestBody.DefaultBmcUsername == nil && tt.requestBody.DefaultBmcPassword == nil {
					assert.Empty(t, capturedProxy.EncryptedSecrets)
				}
			}
			if tt.rejectsCredentials {
				after, readErr := epsDAO.Get(ctx, nil, testEPS.ID, nil, false)
				require.NoError(t, readErr)
				assert.Equal(t, before, after, "invalid credentials must not mutate the cloud row")
			}

			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}
			if tt.expectedErrorMsg != "" {
				assert.Contains(t, rec.Body.String(), tt.expectedErrorMsg)
			}
			if tt.expectedBmcMac != "" {
				var response model.APIExpectedPowerShelf
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.Equal(t, tt.expectedBmcMac, response.BmcMacAddress)
			}
			if tt.expectedStoredBmcMac != "" {
				stored, err := epsDAO.Get(ctx, nil, testEPS.ID, nil, false)
				assert.Nil(t, err)
				assert.Equal(t, tt.expectedStoredBmcMac, stored.BmcMacAddress)
			}
			if tt.expectNoWorkflow {
				assert.Len(t, mockTemporalClient.Calls, workflowCallsBefore)
			}
		})
	}
}

func TestDeleteExpectedPowerShelfHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site := testExpectedPowerShelfSetupTestData(t, dbSession, org)

	unmanagedIP := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "unmanaged-provider",
		Org:  "other-org",
	}
	_, err := dbSession.DB.NewInsert().Model(unmanagedIP).Exec(ctx)
	assert.Nil(t, err)

	unmanagedSite := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "unmanaged-site",
		Org:                      "other-org",
		InfrastructureProviderID: unmanagedIP.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSite).Exec(ctx)
	assert.Nil(t, err)

	// Create a test ExpectedPowerShelf on managed site (to be deleted)
	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	testEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "00:11:22:33:44:FF",
		ShelfSerialNumber:    "DELETE-SHELF-123",
	})
	assert.Nil(t, err)
	assert.NotNil(t, testEPS)

	// Create a test ExpectedPowerShelf on unmanaged site (should not be deletable)
	unmanagedEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               unmanagedSite.ID,
		BmcMacAddress:        "00:11:22:33:55:00",
		ShelfSerialNumber:    "UNMANAGED-DELETE-456",
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedEPS)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteExpectedPowerShelf", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewDeleteExpectedPowerShelfHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.ProviderAdminRole},
				},
			},
		}
	}

	tests := []struct {
		name           string
		id             string
		setupContext   func(c echo.Context)
		expectedStatus int
	}{
		{
			name: "successful delete",
			id:   testEPS.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testEPS.ID.String())
			},
			expectedStatus: http.StatusNoContent,
		},
		{
			name: "cannot delete on unmanaged site",
			id:   unmanagedEPS.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedEPS.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/expected-power-shelf/" + tt.id
			req := httptest.NewRequest(http.MethodDelete, url, nil)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			err := handler.Handle(c)

			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}
		})
	}
}
