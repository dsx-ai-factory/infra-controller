// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

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
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// testSkuInitDB initializes a test database session (pattern from tenant_test.go)
func testSkuInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// testSkuSetupSchema resets the required tables for SKU tests (pattern from tenant_test.go)
func testSkuSetupSchema(t *testing.T, dbSession *cdb.Session) {
	ctx := context.Background()

	// Reset child tables first
	err := dbSession.DB.ResetModel(ctx, (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.SKU)(nil))
	assert.Nil(t, err)

	// Reset parent tables
	err = dbSession.DB.ResetModel(ctx, (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.Site)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
}

// testSkuSetupTestData creates test infrastructure provider, site, and SKUs
func testSkuSetupTestData(t *testing.T, dbSession *cdb.Session, org string) (*cdbm.InfrastructureProvider, *cdbm.Site, *cdbm.SKU, *cdbm.SKU) {
	ctx := context.Background()

	// Create infrastructure provider
	ip := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "test-provider",
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(ctx)
	assert.Nil(t, err)

	// Create site
	site := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site",
		Org:                      org,
		InfrastructureProviderID: ip.ID,
		Status:                   cdbm.SiteStatusRegistered,
	}
	_, err = dbSession.DB.NewInsert().Model(site).Exec(ctx)
	assert.Nil(t, err)

	// Create test SKUs
	deviceType1 := "gpu-server"
	sku1 := &cdbm.SKU{
		ID:                   "test-sku-1",
		SiteID:               site.ID,
		Description:          "First test SKU",
		DeviceType:           &deviceType1,
		AssociatedMachineIds: []string{"machine-1", "machine-2"},
	}
	_, err = dbSession.DB.NewInsert().Model(sku1).Exec(ctx)
	assert.Nil(t, err)

	deviceType2 := "cpu-server"
	sku2 := &cdbm.SKU{
		ID:                   "test-sku-2",
		SiteID:               site.ID,
		Description:          "Second test SKU",
		DeviceType:           &deviceType2,
		AssociatedMachineIds: []string{"machine-3"},
	}
	_, err = dbSession.DB.NewInsert().Model(sku2).Exec(ctx)
	assert.Nil(t, err)

	return ip, site, sku1, sku2
}

func TestGetAllSkuHandler_Handle(t *testing.T) {
	// Setup
	e := echo.New()
	dbSession := testSkuInitDB(t)
	defer dbSession.Close()

	testSkuSetupSchema(t, dbSession)

	ctx := context.Background()
	cfg := &config.Config{}
	handler := NewGetAllSkuHandler(dbSession, nil, cfg)

	org := "test-org"
	infraProv, site, sku1, sku2 := testSkuSetupTestData(t, dbSession, org)

	// Create an unmanaged site
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

	// Create SKU on unmanaged site
	deviceType := "storage-server"
	unmanagedSku := &cdbm.SKU{
		ID:         "unmanaged-sku",
		SiteID:     unmanagedSite.ID,
		DeviceType: &deviceType,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSku).Exec(ctx)
	assert.Nil(t, err)

	// Helper function to create mock user with provider role
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

	// Helper function to create mock user with tenant role
	createTenantMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-tenant-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          456,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.TenantAdminRole},
				},
			},
		}
	}

	// Create tenant with TargetedInstanceCreation capability
	tenantOrg := "test-tenant-org"
	tenantWithCapability := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant",
		Org:  tenantOrg,
	}
	_, err = dbSession.DB.NewInsert().Model(tenantWithCapability).Exec(ctx)
	assert.Nil(t, err)

	// Create tenant account linking tenant to infrastructure provider
	tenantAccount := &cdbm.TenantAccount{
		ID:                       uuid.New(),
		AccountNumber:            "test-account-123",
		TenantID:                 &tenantWithCapability.ID,
		TenantOrg:                tenantOrg,
		InfrastructureProviderID: infraProv.ID,
		Status:                   cdbm.TenantAccountStatusReady,
		Config:                   cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
	}
	_, err = dbSession.DB.NewInsert().Model(tenantAccount).Exec(ctx)
	assert.Nil(t, err)

	// Create tenant WITHOUT TargetedInstanceCreation capability
	tenantOrgNoCapability := "test-tenant-org-no-capability"
	tenantWithoutCapability := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant-no-capability",
		Org:  tenantOrgNoCapability,
	}
	_, err = dbSession.DB.NewInsert().Model(tenantWithoutCapability).Exec(ctx)
	assert.Nil(t, err)

	// Give this tenant a Ready account whose capability is explicitly disabled,
	// isolating the no-capability authorization path.
	tenantAccountWithoutCapability := &cdbm.TenantAccount{
		ID:                       uuid.New(),
		AccountNumber:            "test-account-no-capability-123",
		TenantID:                 &tenantWithoutCapability.ID,
		TenantOrg:                tenantOrgNoCapability,
		InfrastructureProviderID: infraProv.ID,
		Status:                   cdbm.TenantAccountStatusReady,
		Config:                   cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
	}
	_, err = dbSession.DB.NewInsert().Model(tenantAccountWithoutCapability).Exec(ctx)
	assert.Nil(t, err)

	// Create a tenant without a TenantAccount, isolating the missing-account path.
	tenantOrgNoAccount := "test-tenant-org-no-account"
	tenantWithoutAccount := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant-no-account",
		Org:  tenantOrgNoAccount,
	}
	_, err = dbSession.DB.NewInsert().Model(tenantWithoutAccount).Exec(ctx)
	assert.Nil(t, err)

	tests := []struct {
		name                 string
		siteId               string
		setupContext         func(c echo.Context)
		expectedStatus       int
		checkResponseContent func(t *testing.T, body []byte)
	}{
		{
			name:   "missing siteId returns bad request",
			siteId: "",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return bad request error
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
				var response []model.APISku
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				// Should return the 2 managed SKUs from the specified site
				assert.Len(t, response, 2)
				// Verify we get results from the specified site only
				for _, sku := range response {
					assert.Equal(t, site.ID.String(), sku.SiteID, "All results should be from the specified site")
					assert.NotEqual(t, unmanagedSku.ID, sku.ID, "Unmanaged SKU should not be in response")
					assert.NotEmpty(t, sku.Description)
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
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return forbidden error
			},
		},
		{
			name:   "missing user context",
			siteId: "",
			setupContext: func(c echo.Context) {
				// Don't set user in context - should cause error
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusInternalServerError,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return internal server error
			},
		},
		{
			name:   "tenant with TargetedInstanceCreation capability can retrieve SKUs",
			siteId: site.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createTenantMockUser(tenantOrg))
				c.SetParamNames("orgName")
				c.SetParamValues(tenantOrg)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APISku
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Len(t, response, 2)
				for _, sku := range response {
					assert.Equal(t, site.ID.String(), sku.SiteID)
				}
			},
		},
		{
			name:   "tenant without TargetedInstanceCreation capability is denied",
			siteId: site.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createTenantMockUser(tenantOrgNoCapability))
				c.SetParamNames("orgName")
				c.SetParamValues(tenantOrgNoCapability)
			},
			expectedStatus: http.StatusForbidden,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return forbidden error
			},
		},
		{
			name:   "tenant without TenantAccount with Provider is denied",
			siteId: site.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createTenantMockUser(tenantOrgNoAccount))
				c.SetParamNames("orgName")
				c.SetParamValues(tenantOrgNoAccount)
			},
			expectedStatus: http.StatusForbidden,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return forbidden error
			},
		},
	}

	_ = infraProv               // Ensure infraProv is used to avoid compiler warning
	_ = sku1                    // Ensure sku1 is used to avoid compiler warning
	_ = sku2                    // Ensure sku2 is used to avoid compiler warning
	_ = tenantAccount           // Ensure tenantAccount is used to avoid compiler warning
	_ = tenantWithoutCapability // Ensure tenantWithoutCapability is used to avoid compiler warning
	_ = tenantWithoutAccount    // Ensure tenantWithoutAccount is used to avoid compiler warning

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/sku"
			if tt.siteId != "" {
				url += "?siteId=" + tt.siteId
			}
			req := httptest.NewRequest(http.MethodGet, url, nil)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			// Setup context
			tt.setupContext(c)

			// Execute
			err := handler.Handle(c)

			// Assert
			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			// Check response content if provided
			if tt.checkResponseContent != nil && rec.Code == http.StatusOK {
				tt.checkResponseContent(t, rec.Body.Bytes())
			}
		})
	}
}

func TestGetSkuHandler_Handle(t *testing.T) {
	// Setup
	e := echo.New()
	dbSession := testSkuInitDB(t)
	defer dbSession.Close()

	testSkuSetupSchema(t, dbSession)

	ctx := context.Background()

	cfg := &config.Config{}
	handler := NewGetSkuHandler(dbSession, nil, cfg)

	org := "test-org"
	infraProv, site, sku1, _ := testSkuSetupTestData(t, dbSession, org)

	// Create an unmanaged site
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

	// Create SKU on unmanaged site
	deviceType := "storage-server"
	unmanagedSku := &cdbm.SKU{
		ID:         "unmanaged-sku-get",
		SiteID:     unmanagedSite.ID,
		DeviceType: &deviceType,
	}
	_, err = dbSession.DB.NewInsert().Model(unmanagedSku).Exec(ctx)
	assert.Nil(t, err)

	// Helper function to create mock user with provider role
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

	// Helper function to create mock user with tenant role
	createTenantMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			StarfleetID: cutil.GetPtr("test-tenant-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          456,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{authz.TenantAdminRole},
				},
			},
		}
	}

	// Create tenant with TargetedInstanceCreation capability
	tenantOrg := "test-tenant-org"
	tenantWithCapability := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant",
		Org:  tenantOrg,
	}
	_, err = dbSession.DB.NewInsert().Model(tenantWithCapability).Exec(ctx)
	assert.Nil(t, err)

	// Create tenant account linking tenant to infrastructure provider
	tenantAccount := &cdbm.TenantAccount{
		ID:                       uuid.New(),
		AccountNumber:            "test-account-456",
		TenantID:                 &tenantWithCapability.ID,
		TenantOrg:                tenantOrg,
		InfrastructureProviderID: infraProv.ID,
		Status:                   cdbm.TenantAccountStatusReady,
		Config:                   cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
	}
	_, err = dbSession.DB.NewInsert().Model(tenantAccount).Exec(ctx)
	assert.Nil(t, err)

	// Create tenant WITHOUT TargetedInstanceCreation capability
	tenantOrgNoCapability := "test-tenant-org-no-capability"
	tenantWithoutCapability := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant-no-capability",
		Org:  tenantOrgNoCapability,
	}
	_, err = dbSession.DB.NewInsert().Model(tenantWithoutCapability).Exec(ctx)
	assert.Nil(t, err)

	// Give this tenant a Ready account whose capability is explicitly disabled,
	// isolating the no-capability authorization path.
	tenantAccountWithoutCapability := &cdbm.TenantAccount{
		ID:                       uuid.New(),
		AccountNumber:            "test-account-no-capability-456",
		TenantID:                 &tenantWithoutCapability.ID,
		TenantOrg:                tenantOrgNoCapability,
		InfrastructureProviderID: infraProv.ID,
		Status:                   cdbm.TenantAccountStatusReady,
		Config:                   cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
	}
	_, err = dbSession.DB.NewInsert().Model(tenantAccountWithoutCapability).Exec(ctx)
	assert.Nil(t, err)

	// Create a tenant without a TenantAccount, isolating the missing-account path.
	tenantOrgNoAccount := "test-tenant-org-no-account"
	tenantWithoutAccount := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant-no-account",
		Org:  tenantOrgNoAccount,
	}
	_, err = dbSession.DB.NewInsert().Model(tenantWithoutAccount).Exec(ctx)
	assert.Nil(t, err)

	tests := []struct {
		name                 string
		id                   string
		setupContext         func(c echo.Context)
		expectedStatus       int
		checkResponseContent func(t *testing.T, body []byte)
	}{
		{
			name: "successful retrieval",
			id:   sku1.ID,
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, sku1.ID)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response model.APISku
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Equal(t, sku1.ID, response.ID, "SKU ID should match")
				assert.Equal(t, site.ID.String(), response.SiteID, "Site ID should match")
				assert.Equal(t, sku1.Description, response.Description, "SKU description should match")
			},
		},
		{
			name: "SKU not found",
			id:   "non-existent-sku-id",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "non-existent-sku-id")
			},
			expectedStatus: http.StatusNotFound,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return not found error
			},
		},
		{
			name: "cannot retrieve from unmanaged site",
			id:   unmanagedSku.ID,
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedSku.ID)
			},
			expectedStatus: http.StatusForbidden,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return forbidden error
			},
		},
		{
			name: "missing user context",
			id:   sku1.ID,
			setupContext: func(c echo.Context) {
				// Don't set user in context - should cause error
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, sku1.ID)
			},
			expectedStatus: http.StatusInternalServerError,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return internal server error
			},
		},
		{
			name: "tenant with TargetedInstanceCreation capability can retrieve SKU",
			id:   sku1.ID,
			setupContext: func(c echo.Context) {
				c.Set("user", createTenantMockUser(tenantOrg))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(tenantOrg, sku1.ID)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response model.APISku
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Equal(t, sku1.ID, response.ID)
				assert.Equal(t, site.ID.String(), response.SiteID)
			},
		},
		{
			name: "tenant without TargetedInstanceCreation capability is denied",
			id:   sku1.ID,
			setupContext: func(c echo.Context) {
				c.Set("user", createTenantMockUser(tenantOrgNoCapability))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(tenantOrgNoCapability, sku1.ID)
			},
			expectedStatus: http.StatusForbidden,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return forbidden error
			},
		},
		{
			name: "tenant without TenantAccount with Provider is denied",
			id:   sku1.ID,
			setupContext: func(c echo.Context) {
				c.Set("user", createTenantMockUser(tenantOrgNoAccount))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(tenantOrgNoAccount, sku1.ID)
			},
			expectedStatus: http.StatusForbidden,
			checkResponseContent: func(t *testing.T, body []byte) {
				// Should return forbidden error
			},
		},
	}

	_ = infraProv               // Ensure infraProv is used to avoid compiler warning
	_ = tenantAccount           // Ensure tenantAccount is used to avoid compiler warning
	_ = tenantWithoutCapability // Ensure tenantWithoutCapability is used to avoid compiler warning
	_ = tenantWithoutAccount    // Ensure tenantWithoutAccount is used to avoid compiler warning

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/sku/" + tt.id
			req := httptest.NewRequest(http.MethodGet, url, nil)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			// Setup context
			tt.setupContext(c)

			// Execute
			err := handler.Handle(c)

			// Assert
			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			// Check response content if provided
			if tt.checkResponseContent != nil && rec.Code == http.StatusOK {
				tt.checkResponseContent(t, rec.Body.Bytes())
			}
		})
	}
}

func TestCreateSkuHandler(t *testing.T) {
	t.Run("proxies create and returns created SKU", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			skipPersistedSKU: true,
		})
		req := validSkuCreateRequest(fixture.siteID)

		rec := fixture.request(t, http.MethodPost, "", req, fixture.createHandler.Handle)
		require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_CreateSku_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.SkuList
		require.NoError(t, protojson.Unmarshal(fixture.requests[0].RequestJSON, &coreReq))
		require.Len(t, coreReq.Skus, 1)
		assert.Equal(t, req.ID, coreReq.Skus[0].Id)
		assert.Equal(t, model.CoreSkuSchemaVersion, coreReq.Skus[0].SchemaVersion)
		require.Len(t, coreReq.Skus[0].Components.Storage, 1)
		assert.Empty(t, coreReq.Skus[0].Components.Storage[0].Vendor)
		assert.Zero(t, coreReq.Skus[0].Components.Storage[0].CapacityMb)
		assert.Equal(t, uint32(3_600_000), coreReq.Skus[0].Components.Storage[0].GetMinSizeMb())
		assert.Equal(t, uint32(3_900_000), coreReq.Skus[0].Components.Storage[0].GetMaxSizeMb())
		assert.Equal(t, []string{`^/devices/pci.*/nvme$`}, coreReq.Skus[0].Components.Storage[0].PciPatterns)

		var response model.APISku
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
		assert.Equal(t, req.ID, response.ID)
		assert.Equal(t, fixture.siteID, response.SiteID)
		assert.Empty(t, response.AssociatedMachineIds)
		require.NotNil(t, response.Created)
		assert.True(t, existingSkuCreatedTime().Equal(*response.Created))

		saved, err := cdbm.NewSkuDAO(fixture.createHandler.dbSession).Get(context.Background(), nil, req.ID)
		require.NoError(t, err)
		assert.Equal(t, uuid.MustParse(fixture.siteID), saved.SiteID)
		assert.Equal(t, response.Description, saved.Description)
		assert.Equal(t, response.SchemaVersion, saved.SchemaVersion)
		assert.Equal(t, response.DeviceType, saved.DeviceType)
		assert.True(t, existingSkuCreatedTime().Round(time.Microsecond).Equal(saved.Created))
		assert.False(t, response.Created.Equal(saved.Created))
		require.NotNil(t, saved.Components)
		require.NotNil(t, saved.Components.Chassis)
		// The post-create Core response is authoritative, even when it differs
		// from the create request used by this test fixture.
		assert.Equal(t, "existing chassis", saved.Components.Chassis.Model)
	})

	t.Run("returns created when post-create fetch fails", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findError:        errors.New("post-create fetch failed"),
			skipPersistedSKU: true,
		})
		req := validSkuCreateRequest(fixture.siteID)

		beforeCreate := time.Now().UTC()
		rec := fixture.request(t, http.MethodPost, "", req, fixture.createHandler.Handle)
		afterCreate := time.Now().UTC()
		require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_CreateSku_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[1].FullMethod)

		var response model.APISku
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
		assert.Equal(t, req.ID, response.ID)
		assert.Equal(t, fixture.siteID, response.SiteID)
		assert.Equal(t, *req.Description, response.Description)
		assert.Equal(t, model.CoreSkuSchemaVersion, response.SchemaVersion)
		assert.Equal(t, req.DeviceType, response.DeviceType)
		assert.Equal(t, model.NewAPISkuComponents(req.Components.ToProto()), response.Components)
		assert.Empty(t, response.AssociatedMachineIds)
		assert.True(t, response.Created.After(beforeCreate))

		saved, err := cdbm.NewSkuDAO(fixture.createHandler.dbSession).Get(context.Background(), nil, req.ID)
		require.NoError(t, err)
		assert.Equal(t, uuid.MustParse(fixture.siteID), saved.SiteID)
		assert.Equal(t, *req.Description, saved.Description)
		assert.Equal(t, model.CoreSkuSchemaVersion, saved.SchemaVersion)
		assert.Equal(t, req.DeviceType, saved.DeviceType)
		assert.False(t, saved.Created.Before(beforeCreate))
		assert.False(t, saved.Created.After(afterCreate))
		require.NotNil(t, saved.Components)
		require.Len(t, saved.Components.Storage, 1)
		assert.Equal(t, uint32(3_600_000), saved.Components.Storage[0].GetMinSizeMb())
		assert.Equal(t, uint32(3_900_000), saved.Components.Storage[0].GetMaxSizeMb())
	})

	t.Run("uses Core as the authority for duplicate IDs", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			createError: status.Error(codes.AlreadyExists, "SKU already exists: sku-1"),
		})

		rec := fixture.request(t, http.MethodPost, "", validSkuCreateRequest(fixture.siteID), fixture.createHandler.Handle)

		require.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 1)
		assert.Equal(t, corev1.Forge_CreateSku_FullMethodName, fixture.requests[0].FullMethod)
	})

	t.Run("returns conflict when the REST projection already exists", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})

		rec := fixture.request(t, http.MethodPost, "", validSkuCreateRequest(fixture.siteID), fixture.createHandler.Handle)

		require.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_CreateSku_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[1].FullMethod)
		assert.Contains(t, rec.Body.String(), "sku-1")
		assert.Contains(t, rec.Body.String(), "inspect")
		assert.Contains(t, rec.Body.String(), "update")
	})

	t.Run("returns error when Core create fails", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			createError:      errors.New("Core unavailable"),
			skipPersistedSKU: true,
		})

		rec := fixture.request(t, http.MethodPost, "", validSkuCreateRequest(fixture.siteID), fixture.createHandler.Handle)

		require.Equal(t, http.StatusInternalServerError, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 1)
		assert.Equal(t, corev1.Forge_CreateSku_FullMethodName, fixture.requests[0].FullMethod)
		_, err := cdbm.NewSkuDAO(fixture.createHandler.dbSession).Get(context.Background(), nil, "sku-1")
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("rejects invalid Site ID", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		req := validSkuCreateRequest("invalid-site-id")

		rec := fixture.request(t, http.MethodPost, "", req, fixture.createHandler.Handle)

		require.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "siteId")
		assert.Empty(t, fixture.requests)
	})

	t.Run("rejects non-empty read-only ethernet devices", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		req := validSkuCreateRequest(fixture.siteID)
		req.Components.EthernetDevices = []model.APISkuEthernetDevice{
			{
				Vendor:      "Mellanox Technologies",
				Model:       "MT2892 Family [ConnectX-6 Dx]",
				Count:       2,
				IsConnected: true,
			},
		}

		rec := fixture.request(t, http.MethodPost, "", req, fixture.createHandler.Handle)

		require.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "ethernetDevices")
		assert.Contains(t, rec.Body.String(), "read-only")
		assert.Empty(t, fixture.requests)
	})

	t.Run("rejects legacy storage mutation fields", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})

		for _, field := range []string{"vendor", "capacityMb"} {
			t.Run(field, func(t *testing.T) {
				rec := fixture.request(t, http.MethodPost, "", map[string]any{
					"siteId": fixture.siteID,
					"id":     "sku-legacy-storage",
					"components": map[string]any{
						"storage": []map[string]any{{
							"model": "legacy",
							"count": 1,
							field:   0,
						}},
					},
				}, fixture.createHandler.Handle)

				assert.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
			})
		}
		assert.Empty(t, fixture.requests)
	})

	t.Run("rejects inverted storage size range", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		req := validSkuCreateRequest(fixture.siteID)
		req.Components.Storage[0].MinSizeMiB = cutil.GetPtr(uint32(4_000_000))
		req.Components.Storage[0].MaxSizeMiB = cutil.GetPtr(uint32(3_800_000))

		rec := fixture.request(t, http.MethodPost, "", req, fixture.createHandler.Handle)

		assert.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "minSizeMiB")
		assert.Empty(t, fixture.requests)
	})

	t.Run("rejects tenant admin", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.TenantAdminRole})

		rec := fixture.request(t, http.MethodPost, "", validSkuCreateRequest(fixture.siteID), fixture.createHandler.Handle)
		assert.Equal(t, http.StatusForbidden, rec.Code)
		assert.Empty(t, fixture.requests)
	})
}

func TestUpdateSkuHandler(t *testing.T) {
	t.Run("uses metadata RPC for metadata patch", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		deviceType := "cpu-server"

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			DeviceType: &deviceType,
		}, fixture.updateHandler.Handle)
		require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_UpdateSkuMetadata_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.SkuUpdateMetadataRequest
		require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
		assert.Equal(t, "sku-1", coreReq.SkuId)
		assert.Equal(t, deviceType, coreReq.GetDeviceType())

		var response model.APISku
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
		require.NotNil(t, response.DeviceType)
		assert.Equal(t, deviceType, *response.DeviceType)
		assert.Equal(t, uint32(4), response.SchemaVersion)
		require.NotNil(t, response.Created)
		assert.True(t, existingSkuCreatedTime().Equal(*response.Created))

		saved, err := cdbm.NewSkuDAO(fixture.updateHandler.dbSession).Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		require.NotNil(t, saved.DeviceType)
		assert.Equal(t, deviceType, *saved.DeviceType)
		assert.Equal(t, uint32(4), saved.SchemaVersion)
		assert.True(t, existingSkuCreatedTime().Round(time.Microsecond).Equal(saved.Created))
		assert.False(t, response.Created.Equal(saved.Created))
		require.NotNil(t, saved.Components)
		require.NotNil(t, saved.Components.Chassis)
		assert.Equal(t, "existing chassis", saved.Components.Chassis.Model)
	})

	t.Run("preserves stored created timestamp when Core omits it", func(t *testing.T) {
		existing := existingSkuProto()
		existing.Created = nil
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findResponse: &corev1.SkuList{Skus: []*corev1.Sku{existing}},
		})
		skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
		beforeUpdate, err := skuDAO.Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		description := "updated description"

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Description: &description,
		}, fixture.updateHandler.Handle)

		require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
		var response model.APISku
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
		assert.Nil(t, response.Created)

		afterUpdate, err := skuDAO.Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		assert.True(t, beforeUpdate.Created.Equal(afterUpdate.Created))
	})

	t.Run("preserves projection when Core metadataupdate fails", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			updateMetadataError: errors.New("Core unavailable"),
		})
		deviceType := "cpu-server"

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			DeviceType: &deviceType,
		}, fixture.updateHandler.Handle)
		require.Equal(t, http.StatusInternalServerError, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_UpdateSkuMetadata_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.SkuUpdateMetadataRequest
		require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
		assert.Equal(t, deviceType, coreReq.GetDeviceType())

		saved, err := cdbm.NewSkuDAO(fixture.updateHandler.dbSession).Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		assert.Nil(t, saved.DeviceType)
		assert.Nil(t, saved.Components)
		assert.Empty(t, saved.AssociatedMachineIds)
	})

	t.Run("does not recreate projection deleted after Core update", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			afterUpdateMetadata: func(dbSession *cdb.Session) {
				skuDAO := cdbm.NewSkuDAO(dbSession)
				err := skuDAO.Delete(context.Background(), nil, "sku-1")
				require.NoError(t, err)
			},
		})
		description := "updated description"

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Description: &description,
		}, fixture.updateHandler.Handle)

		require.Equal(t, http.StatusInternalServerError, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "failed to update REST DB")
		_, err := cdbm.NewSkuDAO(fixture.updateHandler.dbSession).Get(context.Background(), nil, "sku-1")
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("replaces version five components", func(t *testing.T) {
		existing := existingSkuProto()
		existing.SchemaVersion = model.CoreSkuSchemaVersion
		deviceType := "cpu-server"
		replacementResponse := existingSkuProto()
		replacementResponse.SchemaVersion = model.CoreSkuSchemaVersion
		replacementResponse.DeviceType = &deviceType
		replacementResponse.Components = validSkuCreateRequest("").ToProto().Skus[0].Components
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findResponse:    &corev1.SkuList{Skus: []*corev1.Sku{existing}},
			replaceResponse: replacementResponse,
		})
		components := validSkuCreateRequest(fixture.siteID).Components

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			DeviceType: &deviceType,
			Components: components,
		}, fixture.updateHandler.Handle)
		require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_ReplaceSku_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.Sku
		require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
		assert.Equal(t, "sku-1", coreReq.Id)
		assert.Equal(t, deviceType, coreReq.GetDeviceType())
		assert.Equal(t, model.CoreSkuSchemaVersion, coreReq.SchemaVersion)
		require.NotNil(t, coreReq.Components)
		require.Len(t, coreReq.Components.Storage, 1)

		saved, err := cdbm.NewSkuDAO(fixture.updateHandler.dbSession).Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		require.NotNil(t, saved.DeviceType)
		assert.Equal(t, deviceType, *saved.DeviceType)
		require.NotNil(t, saved.Components)
		require.Len(t, saved.Components.Storage, 1)
		assert.Equal(t, uint32(3_600_000), saved.Components.Storage[0].GetMinSizeMb())
		assert.Equal(t, uint32(3_900_000), saved.Components.Storage[0].GetMaxSizeMb())
	})

	t.Run("preserves projection when Core replace fails", func(t *testing.T) {
		existing := existingSkuProto()
		existing.SchemaVersion = model.CoreSkuSchemaVersion
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findResponse: &corev1.SkuList{Skus: []*corev1.Sku{existing}},
			replaceError: errors.New("Core unavailable"),
		})
		deviceType := "cpu-server"

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			DeviceType: &deviceType,
			Components: validSkuCreateRequest(fixture.siteID).Components,
		}, fixture.updateHandler.Handle)

		require.Equal(t, http.StatusInternalServerError, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_ReplaceSku_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.Sku
		require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
		assert.Equal(t, deviceType, coreReq.GetDeviceType())

		saved, err := cdbm.NewSkuDAO(fixture.updateHandler.dbSession).Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		assert.Nil(t, saved.DeviceType)
		assert.Nil(t, saved.Components)
	})

	t.Run("replaces components for SKU with associated machines", func(t *testing.T) {
		existing := existingSkuProto()
		existing.SchemaVersion = model.CoreSkuSchemaVersion
		existing.AssociatedMachineIds = []*corev1.MachineId{{Id: "machine-1"}}
		replacementResponse := proto.Clone(existing).(*corev1.Sku)
		replacementResponse.Components = validSkuCreateRequest("").ToProto().Skus[0].Components
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findResponse:    &corev1.SkuList{Skus: []*corev1.Sku{existing}},
			replaceResponse: replacementResponse,
		})

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Components: validSkuCreateRequest(fixture.siteID).Components,
		}, fixture.updateHandler.Handle)

		require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_ReplaceSku_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.Sku
		require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
		require.Len(t, coreReq.AssociatedMachineIds, 1)
		assert.Equal(t, "machine-1", coreReq.AssociatedMachineIds[0].GetId())

		var response model.APISku
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
		assert.Equal(t, []string{"machine-1"}, response.AssociatedMachineIds)

		saved, err := cdbm.NewSkuDAO(fixture.updateHandler.dbSession).Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		assert.Equal(t, []string{"machine-1"}, saved.AssociatedMachineIds)
	})

	t.Run("sends version five when replacing legacy SKU components", func(t *testing.T) {
		existing := existingSkuProto()
		require.Equal(t, uint32(4), existing.SchemaVersion)
		replacementResponse := proto.Clone(existing).(*corev1.Sku)
		replacementResponse.SchemaVersion = model.CoreSkuSchemaVersion
		replacementResponse.Components = validSkuCreateRequest("").ToProto().Skus[0].Components
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findResponse:    &corev1.SkuList{Skus: []*corev1.Sku{existing}},
			replaceResponse: replacementResponse,
		})
		components := validSkuCreateRequest(fixture.siteID).Components

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Components: components,
		}, fixture.updateHandler.Handle)
		require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 2)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
		assert.Equal(t, corev1.Forge_ReplaceSku_FullMethodName, fixture.requests[1].FullMethod)

		var coreReq corev1.Sku
		require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
		assert.Equal(t, model.CoreSkuSchemaVersion, coreReq.SchemaVersion)
		assert.Equal(t, "sku-1", coreReq.Id)
		require.NotNil(t, coreReq.Components)
		require.Len(t, coreReq.Components.Storage, 1)
		assert.True(t, proto.Equal(components.ToProto(), coreReq.Components))
	})

	t.Run("rejects inverted storage size range", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		components := validSkuCreateRequest(fixture.siteID).Components
		components.Storage[0].MinSizeMiB = cutil.GetPtr(uint32(4_000_000))
		components.Storage[0].MaxSizeMiB = cutil.GetPtr(uint32(3_800_000))

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Components: components,
		}, fixture.updateHandler.Handle)

		assert.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "minSizeMiB")
		assert.Empty(t, fixture.requests)
	})

	t.Run("rejects non-empty read-only ethernet devices", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		components := validSkuCreateRequest(fixture.siteID).Components
		components.EthernetDevices = []model.APISkuEthernetDevice{
			{
				Vendor:      "Mellanox Technologies",
				Model:       "MT2892 Family [ConnectX-6 Dx]",
				Count:       2,
				IsConnected: true,
			},
		}

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Components: components,
		}, fixture.updateHandler.Handle)

		require.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "ethernetDevices")
		assert.Contains(t, rec.Body.String(), "read-only")
		assert.Empty(t, fixture.requests)
	})

	t.Run("returns Core not found without completing update", func(t *testing.T) {
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			findResponse: &corev1.SkuList{},
		})
		description := "updated description"

		rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
			Description: &description,
		}, fixture.updateHandler.Handle)
		require.Equal(t, http.StatusNotFound, rec.Code, rec.Body.String())
		assert.JSONEq(t, `{"source":"","message":"Could not find SKU with the specified ID","data":null}`, rec.Body.String())
		require.Len(t, fixture.requests, 1)
		assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
	})

	t.Run("returns not found for SKU not present in REST database", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		description := "updated description"

		rec := fixture.request(t, http.MethodPatch, "missing-sku", model.APISkuUpdateRequest{
			Description: &description,
		}, fixture.updateHandler.Handle)
		require.Equal(t, http.StatusNotFound, rec.Code, rec.Body.String())
		assert.JSONEq(t, `{"source":"","message":"Could not find SKU with the specified ID","data":null}`, rec.Body.String())
		assert.Empty(t, fixture.requests)
	})

}

func TestDeleteSkuHandler(t *testing.T) {
	t.Run("proxies delete", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})

		rec := fixture.request(t, http.MethodDelete, "sku-1", nil, fixture.deleteHandler.Handle)
		require.Equal(t, http.StatusNoContent, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 1)
		assert.Equal(t, corev1.Forge_DeleteSku_FullMethodName, fixture.requests[0].FullMethod)

		var coreReq corev1.SkuIdList
		require.NoError(t, protojson.Unmarshal(fixture.requests[0].RequestJSON, &coreReq))
		assert.Equal(t, []string{"sku-1"}, coreReq.Ids)
		_, err := cdbm.NewSkuDAO(fixture.deleteHandler.dbSession).Get(context.Background(), nil, "sku-1")
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("rejects SKU with associated machines", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
		skuDAO := cdbm.NewSkuDAO(fixture.deleteHandler.dbSession)
		_, err := skuDAO.Update(context.Background(), nil, cdbm.SkuUpdateInput{
			SkuID:                "sku-1",
			AssociatedMachineIds: []string{"machine-1"},
		})
		require.NoError(t, err)

		rec := fixture.request(t, http.MethodDelete, "sku-1", nil, fixture.deleteHandler.Handle)
		require.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.JSONEq(t, `{"source":"","message":"SKU is associated with machines and cannot be deleted","data":null}`, rec.Body.String())
		assert.Empty(t, fixture.requests)

		saved, err := skuDAO.Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		assert.Equal(t, []string{"machine-1"}, saved.AssociatedMachineIds)
	})

	t.Run("removes stale record when Core returns not found", func(t *testing.T) {
		deleteErr := tp.NewApplicationErrorWithCause(
			"SKU not found",
			swe.ErrTypeNICoObjectNotFound,
			status.Error(codes.NotFound, "SKU not found"),
		)
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			deleteError: deleteErr,
		})

		rec := fixture.request(t, http.MethodDelete, "sku-1", nil, fixture.deleteHandler.Handle)
		require.Equal(t, http.StatusNoContent, rec.Code, rec.Body.String())
		require.Len(t, fixture.requests, 1)
		assert.Equal(t, corev1.Forge_DeleteSku_FullMethodName, fixture.requests[0].FullMethod)
		_, err := cdbm.NewSkuDAO(fixture.deleteHandler.dbSession).Get(context.Background(), nil, "sku-1")
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("preserves record when Core delete fails", func(t *testing.T) {
		deleteErr := tp.NewApplicationErrorWithCause(
			"Core unavailable",
			swe.ErrTypeNICoUnavailable,
			status.Error(codes.Unavailable, "Core unavailable"),
		)
		fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
			deleteError: deleteErr,
		})

		rec := fixture.request(t, http.MethodDelete, "sku-1", nil, fixture.deleteHandler.Handle)
		require.Equal(t, http.StatusServiceUnavailable, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "Core unavailable")
		require.Len(t, fixture.requests, 1)
		assert.Equal(t, corev1.Forge_DeleteSku_FullMethodName, fixture.requests[0].FullMethod)
		_, err := cdbm.NewSkuDAO(fixture.deleteHandler.dbSession).Get(context.Background(), nil, "sku-1")
		assert.NoError(t, err)
	})

	t.Run("returns not found for unsaved SKU", func(t *testing.T) {
		fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})

		rec := fixture.request(t, http.MethodDelete, "missing-sku", nil, fixture.deleteHandler.Handle)
		require.Equal(t, http.StatusNotFound, rec.Code, rec.Body.String())
		assert.JSONEq(t, `{"source":"","message":"Could not find SKU with the specified ID","data":null}`, rec.Body.String())
		assert.Empty(t, fixture.requests)
	})
}

type skuManagementFixture struct {
	org           string
	siteID        string
	user          *cdbm.User
	createHandler CreateSkuHandler
	updateHandler UpdateSkuHandler
	deleteHandler DeleteSkuHandler
	requests      []grpcproxy.Request
}

type skuManagementFixtureOptions struct {
	findResponse        *corev1.SkuList
	findError           error
	createError         error
	replaceResponse     *corev1.Sku
	replaceError        error
	updateMetadataError error
	deleteError         error
	skipPersistedSKU    bool
	afterUpdateMetadata func(*cdb.Session)
}

func newSkuManagementFixture(t *testing.T, roles []string) *skuManagementFixture {
	return newSkuManagementFixtureWithOptions(t, roles, skuManagementFixtureOptions{})
}

func newSkuManagementFixtureWithOptions(t *testing.T, roles []string, options skuManagementFixtureOptions) *skuManagementFixture {
	t.Helper()
	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.SKU)(nil)))

	org := "test-org"
	user := common.TestBuildUser(t, dbSession, uuid.NewString(), org, roles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", org, user)
	site := common.TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	if !options.skipPersistedSKU {
		skuDAO := cdbm.NewSkuDAO(dbSession)
		_, err = skuDAO.Create(context.Background(), nil, cdbm.SkuCreateInput{
			SkuID:         "sku-1",
			SiteID:        site.ID,
			SchemaVersion: 4,
		})
		require.NoError(t, err)
	}

	fixture := &skuManagementFixture{org: org, siteID: site.ID.String(), user: user}
	client := &tmocks.Client{}
	existing := existingSkuProto()
	if options.createError != nil {
		fixture.addWorkflowError(client, corev1.Forge_CreateSku_FullMethodName, options.createError)
	} else {
		fixture.addWorkflow(t, client, corev1.Forge_CreateSku_FullMethodName, &corev1.SkuIdList{Ids: []string{"sku-1"}})
	}
	if options.findError != nil {
		fixture.addWorkflowError(client, corev1.Forge_FindSkusByIds_FullMethodName, options.findError)
	} else {
		findResponse := options.findResponse
		if findResponse == nil {
			findResponse = &corev1.SkuList{Skus: []*corev1.Sku{existing}}
		}
		fixture.addWorkflow(t, client, corev1.Forge_FindSkusByIds_FullMethodName, findResponse)
	}
	replaceResponse := options.replaceResponse
	if replaceResponse == nil {
		replaceResponse = existing
	}
	if options.replaceError != nil {
		fixture.addWorkflowError(client, corev1.Forge_ReplaceSku_FullMethodName, options.replaceError)
	} else {
		fixture.addWorkflow(t, client, corev1.Forge_ReplaceSku_FullMethodName, replaceResponse)
	}
	if options.updateMetadataError != nil {
		fixture.addWorkflowError(client, corev1.Forge_UpdateSkuMetadata_FullMethodName, options.updateMetadataError)
	} else {
		fixture.addWorkflow(t, client, corev1.Forge_UpdateSkuMetadata_FullMethodName, nil, func() {
			if options.afterUpdateMetadata != nil {
				options.afterUpdateMetadata(dbSession)
			}
		})
	}
	if options.deleteError != nil {
		fixture.addWorkflowError(client, corev1.Forge_DeleteSku_FullMethodName, options.deleteError)
	} else {
		fixture.addWorkflow(t, client, corev1.Forge_DeleteSku_FullMethodName, nil)
	}

	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = client
	fixture.createHandler = NewCreateSkuHandler(dbSession, scp)
	fixture.updateHandler = NewUpdateSkuHandler(dbSession, scp)
	fixture.deleteHandler = NewDeleteSkuHandler(dbSession, scp)
	return fixture
}

func (f *skuManagementFixture) addWorkflowError(client *tmocks.Client, method string, getErr error) {
	run := &tmocks.WorkflowRun{}
	run.On("Get", mock.Anything, mock.Anything).Return(getErr)
	client.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool { return req.FullMethod == method }),
	).Run(func(args mock.Arguments) {
		f.requests = append(f.requests, args.Get(3).(grpcproxy.Request))
	}).Return(run, nil).Maybe()
}

func (f *skuManagementFixture) addWorkflow(t *testing.T, client *tmocks.Client, method string, response proto.Message, afterGet ...func()) {
	t.Helper()
	run := &tmocks.WorkflowRun{}
	var responseJSON []byte
	if response != nil {
		var err error
		responseJSON, err = protojson.Marshal(response)
		require.NoError(t, err)
	}
	run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		out, ok := args.Get(1).(*grpcproxy.Response)
		require.True(t, ok)
		out.ResponseJSON = responseJSON
		for _, callback := range afterGet {
			callback()
		}
	}).Return(nil)
	client.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool { return req.FullMethod == method }),
	).Run(func(args mock.Arguments) {
		f.requests = append(f.requests, args.Get(3).(grpcproxy.Request))
	}).Return(run, nil).Maybe()
}

func (f *skuManagementFixture) request(t *testing.T, method, skuID string, body any, handler func(echo.Context) error) *httptest.ResponseRecorder {
	t.Helper()
	requestJSON, err := json.Marshal(body)
	require.NoError(t, err)
	req := httptest.NewRequest(method, "/", strings.NewReader(string(requestJSON)))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := echo.New().NewContext(req, rec)
	ec.SetParamNames("orgName", "id")
	ec.SetParamValues(f.org, skuID)
	ec.Set("user", f.user)
	require.NoError(t, handler(ec))
	return rec
}

func validSkuCreateRequest(siteID string) model.APISkuCreateRequest {
	deviceType := "gpu-server"
	return model.APISkuCreateRequest{
		SiteID:      siteID,
		ID:          "sku-1",
		Description: cutil.GetPtr("test SKU"),
		DeviceType:  &deviceType,
		Components: &model.APISkuComponents{
			Chassis: &model.APISkuChassis{
				Vendor:       "NVIDIA",
				Model:        "DGX H100",
				Architecture: "x86_64",
			},
			Storage: []model.APISkuStorage{{
				Model:       "informational-model",
				Count:       2,
				MinSizeMiB:  cutil.GetPtr(uint32(3_600_000)),
				MaxSizeMiB:  cutil.GetPtr(uint32(3_900_000)),
				PciPatterns: []string{`^/devices/pci.*/nvme$`},
			}},
		},
	}
}

func existingSkuProto() *corev1.Sku {
	description := "old description"
	deviceType := "gpu-server"
	return &corev1.Sku{
		Id:            "sku-1",
		Description:   &description,
		Created:       timestamppb.New(existingSkuCreatedTime()),
		SchemaVersion: 4,
		DeviceType:    &deviceType,
		Components: &corev1.SkuComponents{
			Chassis: &corev1.SkuComponentChassis{
				Vendor:       "NVIDIA",
				Model:        "existing chassis",
				Architecture: "x86_64",
			},
		},
	}
}

func existingSkuCreatedTime() time.Time {
	return time.Date(2025, time.January, 2, 3, 4, 5, 678_901_234, time.UTC)
}
