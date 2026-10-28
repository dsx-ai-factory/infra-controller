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
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
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
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
	temporalEnums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/structpb"
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
		orderBy              string
		setupContext         func(c echo.Context)
		expectedStatus       int
		expectedPagination   string
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
			name:    "successful GetAll ordered by ID ascending",
			siteId:  site.ID.String(),
			orderBy: "ID_ASC",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus:     http.StatusOK,
			expectedPagination: `{"pageNumber":1,"pageSize":20,"total":2,"orderBy":"ID_ASC"}`,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APISku
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Len(t, response, 2)
				assert.Equal(t, sku1.ID, response[0].ID)
				assert.Equal(t, sku2.ID, response[1].ID)
			},
		},
		{
			name:    "successful GetAll ordered by ID descending",
			siteId:  site.ID.String(),
			orderBy: "ID_DESC",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus:     http.StatusOK,
			expectedPagination: `{"pageNumber":1,"pageSize":20,"total":2,"orderBy":"ID_DESC"}`,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APISku
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Len(t, response, 2)
				assert.Equal(t, sku2.ID, response[0].ID)
				assert.Equal(t, sku1.ID, response[1].ID)
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
			if tt.orderBy != "" {
				url += "&orderBy=" + tt.orderBy
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
			if tt.expectedPagination != "" {
				assert.JSONEq(t, tt.expectedPagination, rec.Header().Get(pagination.ResponseHeaderName))
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

func TestUpdateSkuHandler_Handle(t *testing.T) {
	cases := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "uses metadata RPC for metadata patch",
			run: func(t *testing.T) {
				existing := existingSkuProto()
				existing.AssociatedMachineIds = []*corev1.MachineId{{Id: "machine-1"}}
				fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
					findResponse: &corev1.SkuList{Skus: []*corev1.Sku{existing}},
				})
				storedDescription := "REST description"
				storedSchemaVersion := uint32(3)
				skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
				_, err := skuDAO.Update(context.Background(), nil, cdbm.SkuUpdateInput{
					SkuID:         "sku-1",
					Description:   &storedDescription,
					SchemaVersion: &storedSchemaVersion,
				})
				require.NoError(t, err)
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
				assert.Nil(t, coreReq.Description)

				var response model.APISku
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				require.NotNil(t, response.DeviceType)
				assert.Equal(t, deviceType, *response.DeviceType)
				assert.Equal(t, storedDescription, response.Description)
				assert.Equal(t, uint32(4), response.SchemaVersion)
				assert.Equal(t, []string{"machine-1"}, response.AssociatedMachineIds)
				require.NotNil(t, response.Created)
				assert.True(t, existingSkuCreatedTime().Equal(*response.Created))

				saved, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				require.NotNil(t, saved.DeviceType)
				assert.Equal(t, deviceType, *saved.DeviceType)
				assert.Equal(t, storedDescription, saved.Description)
				assert.Equal(t, uint32(4), saved.SchemaVersion)
				assert.Equal(t, []string{"machine-1"}, saved.AssociatedMachineIds)
				assert.True(t, existingSkuCreatedTime().Round(time.Microsecond).Equal(saved.Created))
				assert.False(t, response.Created.Equal(saved.Created))
				require.NotNil(t, saved.Components)
				require.NotNil(t, saved.Components.Chassis)
				assert.Equal(t, "existing chassis", saved.Components.Chassis.Model)
			},
		},
		{
			name: "preserves independent metadata patches from the same Core snapshot",
			run: func(t *testing.T) {
				initial := existingSkuProto()
				cases := []struct {
					name           string
					outerRequest   model.APISkuUpdateRequest
					nestedRequest  model.APISkuUpdateRequest
					nestedMetadata model.APISku
					finalMetadata  model.APISku
					outerNoOp      bool
				}{
					{
						name:           "device type patch preserves cleared description",
						outerRequest:   model.APISkuUpdateRequest{DeviceType: cutil.GetPtr("cpu-server")},
						nestedRequest:  model.APISkuUpdateRequest{Description: cutil.GetPtr("")},
						nestedMetadata: model.APISku{Description: "", DeviceType: initial.DeviceType},
						finalMetadata:  model.APISku{Description: "", DeviceType: cutil.GetPtr("cpu-server")},
					},
					{
						name:           "unchanged description patch preserves cleared device type without another write",
						outerRequest:   model.APISkuUpdateRequest{Description: initial.Description},
						nestedRequest:  model.APISkuUpdateRequest{DeviceType: cutil.GetPtr("")},
						nestedMetadata: model.APISku{Description: initial.GetDescription(), DeviceType: cutil.GetPtr("")},
						finalMetadata:  model.APISku{Description: initial.GetDescription(), DeviceType: cutil.GetPtr("")},
						outerNoOp:      true,
					},
				}
				for _, tc := range cases {
					t.Run(tc.name, func(t *testing.T) {
						var fixture *skuManagementFixture
						var nestedSaved *cdbm.SKU
						metadataUpdates := 0
						fixture = newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
							findResponse: &corev1.SkuList{Skus: []*corev1.Sku{initial}},
							afterUpdateMetadata: func(dbSession *cdb.Session) {
								metadataUpdates++
								// The nested PATCH re-enters this callback; run it only for the outer RPC.
								if metadataUpdates != 1 {
									return
								}

								// Both requests read the same captured Core snapshot. Finish the
								// nested PATCH while the outer RPC is pending, before its REST save.
								rec := fixture.request(t, http.MethodPatch, "sku-1", tc.nestedRequest, fixture.updateHandler.Handle)
								require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
								var response model.APISku
								require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
								assert.Equal(t, tc.nestedMetadata.Description, response.Description)
								assert.Equal(t, tc.nestedMetadata.DeviceType, response.DeviceType)

								var err error
								nestedSaved, err = cdbm.NewSkuDAO(dbSession).Get(context.Background(), nil, "sku-1")
								require.NoError(t, err)
							},
						})
						skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
						created := initial.Created.AsTime()
						// Match the nonmetadata snapshot and start with populated metadata so
						// clears are observable and an unchanged outer PATCH can be a no-op.
						_, err := skuDAO.Update(context.Background(), nil, cdbm.SkuUpdateInput{
							SkuID:         "sku-1",
							Description:   initial.Description,
							DeviceType:    initial.DeviceType,
							SchemaVersion: &initial.SchemaVersion,
							Created:       &created,
							Components:    &cdbm.SkuComponents{SkuComponents: initial.Components},
						})
						require.NoError(t, err)

						rec := fixture.request(t, http.MethodPatch, "sku-1", tc.outerRequest, fixture.updateHandler.Handle)
						require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
						require.Equal(t, 2, metadataUpdates)
						require.Len(t, fixture.requests, 4)
						assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[0].FullMethod)
						assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[2].FullMethod)
						for _, call := range []struct {
							index   int
							request *corev1.SkuUpdateMetadataRequest
						}{
							{1, &corev1.SkuUpdateMetadataRequest{SkuId: "sku-1", Description: tc.outerRequest.Description, DeviceType: tc.outerRequest.DeviceType}},
							{3, &corev1.SkuUpdateMetadataRequest{SkuId: "sku-1", Description: tc.nestedRequest.Description, DeviceType: tc.nestedRequest.DeviceType}},
						} {
							assert.Equal(t, corev1.Forge_UpdateSkuMetadata_FullMethodName, fixture.requests[call.index].FullMethod)
							var request corev1.SkuUpdateMetadataRequest
							require.NoError(t, protojson.Unmarshal(fixture.requests[call.index].RequestJSON, &request))
							assert.True(t, proto.Equal(call.request, &request), "metadata request %d: %v", call.index, &request)
						}

						var response model.APISku
						require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
						assert.Equal(t, tc.finalMetadata.Description, response.Description)
						assert.Equal(t, tc.finalMetadata.DeviceType, response.DeviceType)

						saved, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)
						assert.Equal(t, tc.finalMetadata.Description, saved.Description)
						assert.Equal(t, tc.finalMetadata.DeviceType, saved.DeviceType)
						assert.Equal(t, initial.SchemaVersion, saved.SchemaVersion)
						require.NotNil(t, saved.Components)
						assert.True(t, proto.Equal(initial.Components, saved.Components.SkuComponents))
						assert.True(t, created.Round(time.Microsecond).Equal(saved.Created))
						if tc.outerNoOp {
							require.NotNil(t, nestedSaved)
							assert.True(t, nestedSaved.Updated.Equal(saved.Updated), "unchanged outer PATCH must not write the row again")
						}

						getHandler := NewGetSkuHandler(fixture.updateHandler.dbSession, nil, &config.Config{})
						getRec := fixture.request(t, http.MethodGet, "sku-1", nil, getHandler.Handle)
						require.Equal(t, http.StatusOK, getRec.Code, getRec.Body.String())
						var getResponse model.APISku
						require.NoError(t, json.Unmarshal(getRec.Body.Bytes(), &getResponse))
						assert.Equal(t, tc.finalMetadata.Description, getResponse.Description)
						assert.Equal(t, tc.finalMetadata.DeviceType, getResponse.DeviceType)
					})
				}
			},
		},
		{
			name: "preserves stored created timestamp when Core omits it",
			run: func(t *testing.T) {
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
				assert.Nil(t, response.DeviceType)

				afterUpdate, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				assert.True(t, beforeUpdate.Created.Equal(afterUpdate.Created))
				assert.Nil(t, afterUpdate.DeviceType)
			},
		},
		{
			name: "preserves projection when Core metadataupdate fails",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "does not recreate projection deleted after Core update",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "replaces components and clears device type",
			run: func(t *testing.T) {
				existing := existingSkuProto()
				deviceType := ""
				replacementResponse := existingSkuProto()
				replacementResponse.SchemaVersion = model.CoreSkuSchemaVersion
				replacementResponse.DeviceType = &deviceType
				replacementResponse.Components = validSkuCreateRequest("").ToProto().Skus[0].Components
				fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
					replaceResponse: replacementResponse,
				})
				skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
				_, err := skuDAO.Update(context.Background(), nil, cdbm.SkuUpdateInput{
					SkuID:      "sku-1",
					DeviceType: existing.DeviceType,
				})
				require.NoError(t, err)
				components := validSkuCreateRequest(fixture.siteID).Components

				rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
					DeviceType: &deviceType,
					Components: components,
				}, fixture.updateHandler.Handle)
				require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
				require.Len(t, fixture.requests, 2)
				assert.Equal(t, corev1.Forge_Version_FullMethodName, fixture.requests[0].FullMethod)
				assert.Equal(t, corev1.Forge_ReplaceSkuComponents_FullMethodName, fixture.requests[1].FullMethod)

				var coreReq corev1.ReplaceSkuComponentsRequest
				require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
				assert.Equal(t, "sku-1", coreReq.SkuId)
				require.NotNil(t, coreReq.DeviceType)
				assert.Equal(t, deviceType, coreReq.GetDeviceType())
				assert.Equal(t, model.CoreSkuSchemaVersion, coreReq.SchemaVersion)
				require.NotNil(t, coreReq.Components)
				require.Len(t, coreReq.Components.Storage, 1)

				var response model.APISku
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, &deviceType, response.DeviceType)

				saved, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				require.NotNil(t, saved.DeviceType)
				assert.Equal(t, deviceType, *saved.DeviceType)
				require.NotNil(t, saved.Components)
				require.Len(t, saved.Components.Storage, 1)
				assert.Equal(t, uint32(3_600_000), saved.Components.Storage[0].GetMinSizeMb())
				assert.Equal(t, uint32(3_900_000), saved.Components.Storage[0].GetMaxSizeMb())
			},
		},
		{
			name: "preserves stored device type and skips writes for unchanged component replacement",
			run: func(t *testing.T) {
				existing := existingSkuProto()
				existing.SchemaVersion = model.CoreSkuSchemaVersion
				existing.DeviceType = nil
				replacementResponse := proto.Clone(existing).(*corev1.Sku)
				components := validSkuCreateRequest("").Components
				replacementResponse.Components = components.ToProto()
				fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
					replaceResponse: replacementResponse,
				})
				skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
				deviceType := "REST device type"
				created := existing.Created.AsTime()
				// Match the other Core fields so the first PATCH must write for the component change alone.
				_, err := skuDAO.Update(context.Background(), nil, cdbm.SkuUpdateInput{
					SkuID:         "sku-1",
					DeviceType:    &deviceType,
					Description:   existing.Description,
					SchemaVersion: cutil.GetPtr(existing.SchemaVersion),
					Components:    &cdbm.SkuComponents{SkuComponents: existing.Components},
					Created:       &created,
				})
				require.NoError(t, err)

				updateRequest := model.APISkuUpdateRequest{
					Components: components,
				}
				rec := fixture.request(t, http.MethodPatch, "sku-1", updateRequest, fixture.updateHandler.Handle)

				require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
				require.Len(t, fixture.requests, 2)
				assert.Equal(t, corev1.Forge_Version_FullMethodName, fixture.requests[0].FullMethod)
				assert.Equal(t, corev1.Forge_ReplaceSkuComponents_FullMethodName, fixture.requests[1].FullMethod)
				var coreReq corev1.ReplaceSkuComponentsRequest
				require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
				assert.Nil(t, coreReq.DeviceType)
				assert.True(t, proto.Equal(replacementResponse.Components, coreReq.Components))

				var response model.APISku
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, &deviceType, response.DeviceType)
				require.NotNil(t, response.Components)
				assert.True(t, proto.Equal(replacementResponse.Components, response.Components.ToProto()))
				require.NotNil(t, response.Created)
				assert.True(t, existingSkuCreatedTime().Equal(*response.Created))

				saved, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				assert.Equal(t, &deviceType, saved.DeviceType)
				require.NotNil(t, saved.Components)
				assert.True(t, proto.Equal(replacementResponse.Components, saved.Components.SkuComponents))
				assert.True(t, existingSkuCreatedTime().Round(time.Microsecond).Equal(saved.Created))
				assert.False(t, response.Created.Equal(saved.Created))

				// Backdate `Updated` so the repeat PATCH proves there is no write,
				// even if both requests finish within the database's timestamp precision.
				_, err = fixture.updateHandler.dbSession.DB.NewUpdate().Model((*cdbm.SKU)(nil)).
					Set("updated = ?", saved.Updated.Add(-time.Hour)).Where("id = ?", saved.ID).Exec(context.Background())
				require.NoError(t, err)
				beforeRepeat, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				require.True(t, beforeRepeat.Updated.Before(saved.Updated))

				rec = fixture.request(t, http.MethodPatch, "sku-1", updateRequest, fixture.updateHandler.Handle)
				require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
				require.Len(t, fixture.requests, 4)
				assert.Equal(t, corev1.Forge_Version_FullMethodName, fixture.requests[2].FullMethod)
				assert.Equal(t, corev1.Forge_ReplaceSkuComponents_FullMethodName, fixture.requests[3].FullMethod)
				var repeatCoreReq corev1.ReplaceSkuComponentsRequest
				require.NoError(t, protojson.Unmarshal(fixture.requests[3].RequestJSON, &repeatCoreReq))
				assert.True(t, proto.Equal(&coreReq, &repeatCoreReq), "repeat PATCH must send the same replacement to Core")

				var repeatResponse model.APISku
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &repeatResponse))
				assert.Equal(t, response, repeatResponse)
				afterRepeat, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				assert.Equal(t, beforeRepeat, afterRepeat, "unchanged component PATCH must preserve the row and Updated")

				getHandler := NewGetSkuHandler(fixture.updateHandler.dbSession, nil, &config.Config{})
				getRec := fixture.request(t, http.MethodGet, "sku-1", nil, getHandler.Handle)
				require.Equal(t, http.StatusOK, getRec.Code, getRec.Body.String())
				var getResponse model.APISku
				require.NoError(t, json.Unmarshal(getRec.Body.Bytes(), &getResponse))
				assert.Equal(t, &deviceType, getResponse.DeviceType)
			},
		},
		{
			name: "does not fall back from component replacement without a confirmed dispatch rejection",
			run: func(t *testing.T) {
				cases := []struct {
					name    string
					err     error
					status  int
					message string
				}{
					{
						name:    "permission denied",
						err:     tp.NewNonRetryableApplicationError("Core permission denied", swe.ErrTypeNICoDenied, nil),
						status:  http.StatusForbidden,
						message: "Core permission denied",
					},
					{
						name:    "Core unavailable",
						err:     errors.New("Core unavailable"),
						status:  http.StatusInternalServerError,
						message: "Core unavailable",
					},
					{
						name:    "proxy execution timeout",
						err:     tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil),
						status:  http.StatusGatewayTimeout,
						message: "Core proxy request timed out",
					},
					{
						name:    "unimplemented mutation",
						err:     tp.NewNonRetryableApplicationError("unsupported method", swe.ErrTypeNICoUnimplemented, nil),
						status:  http.StatusNotImplemented,
						message: "unsupported method",
					},
					{
						name:    "SKU no longer exists in Core",
						err:     tp.NewNonRetryableApplicationError("SKU sku-1 not found", swe.ErrTypeNICoObjectNotFound, nil),
						status:  http.StatusNotFound,
						message: "Could not find SKU with the specified ID",
					},
				}
				for _, tc := range cases {
					t.Run(tc.name, func(t *testing.T) {
						fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
							replaceComponentsError: skuProxyActivityError("InvokeCoreGRPCOnSite", tc.err),
						})
						skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
						before, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)

						rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
							DeviceType: cutil.GetPtr("cpu-server"),
							Components: validSkuCreateRequest(fixture.siteID).Components,
						}, fixture.updateHandler.Handle)

						require.Equal(t, tc.status, rec.Code, rec.Body.String())
						var response cutil.APIError
						require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
						assert.Contains(t, response.Message, tc.message)
						assert.Nil(t, response.Data)
						if tc.status == http.StatusNotFound {
							assert.Equal(t, tc.message, response.Message)
						}
						require.Len(t, fixture.requests, 2)
						assert.Equal(t, corev1.Forge_Version_FullMethodName, fixture.requests[0].FullMethod)
						assert.Equal(t, corev1.Forge_ReplaceSkuComponents_FullMethodName, fixture.requests[1].FullMethod)
						after, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)
						assert.Equal(t, before, after, "failed replacement must not change the REST projection")
					})
				}
			},
		},
		{
			name: "uses legacy replacement when capability discovery cannot confirm support",
			run: func(t *testing.T) {
				cases := []struct {
					name                string
					versionResponseJSON string
					versionError        error
					findError           error
					replaceError        error
					status              int
					requests            int
					errorMessage        string
				}{
					{
						name:                "capability absent on older Core",
						versionResponseJSON: `{"capabilities":["BUILD_CAPABILITY_VPC_SLAAC"]}`,
						status:              http.StatusOK,
						requests:            3,
					},
					{
						name:                "numeric capability from older Site Agent",
						versionResponseJSON: `{"capabilities":["BUILD_CAPABILITY_VPC_SLAAC",2]}`,
						status:              http.StatusOK,
						requests:            3,
					},
					{
						name:         "read-only capability probe fails",
						versionError: errors.New("Core version unavailable"),
						status:       http.StatusOK,
						requests:     3,
					},
					{
						name:                "legacy read fails",
						versionResponseJSON: `{}`,
						findError:           errors.New("Core read failed"),
						errorMessage:        "Core read failed",
						status:              http.StatusInternalServerError,
						requests:            2,
					},
					{
						name:                "legacy replacement fails",
						versionResponseJSON: `{}`,
						replaceError:        errors.New("Core replace failed"),
						errorMessage:        "Core replace failed",
						status:              http.StatusInternalServerError,
						requests:            3,
					},
				}
				for _, tc := range cases {
					t.Run(tc.name, func(t *testing.T) {
						existing := existingSkuProto()
						require.Equal(t, uint32(4), existing.SchemaVersion)
						components := validSkuCreateRequest("").Components
						replacementResponse := proto.Clone(existing).(*corev1.Sku)
						replacementResponse.Description = cutil.GetPtr("")
						replacementResponse.SchemaVersion = model.CoreSkuSchemaVersion
						replacementResponse.Components = components.ToProto()
						replacementResponse.AssociatedMachineIds = []*corev1.MachineId{{Id: "machine-1"}}
						fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
							versionResponseJSON: []byte(tc.versionResponseJSON),
							versionError:        tc.versionError,
							findResponse:        &corev1.SkuList{Skus: []*corev1.Sku{existing}},
							findError:           tc.findError,
							replaceResponse:     replacementResponse,
							replaceError:        tc.replaceError,
						})
						skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
						before, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)

						rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
							Description: cutil.GetPtr(""),
							Components:  components,
						}, fixture.updateHandler.Handle)

						require.Equal(t, tc.status, rec.Code, rec.Body.String())
						require.Len(t, fixture.requests, tc.requests)
						assert.Equal(t, corev1.Forge_Version_FullMethodName, fixture.requests[0].FullMethod)
						var versionReq corev1.VersionRequest
						require.NoError(t, protojson.Unmarshal(fixture.requests[0].RequestJSON, &versionReq))
						assert.True(t, proto.Equal(&corev1.VersionRequest{}, &versionReq))
						assert.Equal(t, corev1.Forge_FindSkusByIds_FullMethodName, fixture.requests[1].FullMethod)
						var findReq corev1.SkusByIdsRequest
						require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &findReq))
						assert.Equal(t, []string{"sku-1"}, findReq.Ids)
						if tc.requests == 3 {
							assert.Equal(t, corev1.Forge_ReplaceSku_FullMethodName, fixture.requests[2].FullMethod)
							var coreReq corev1.Sku
							require.NoError(t, protojson.Unmarshal(fixture.requests[2].RequestJSON, &coreReq))
							assert.Equal(t, "sku-1", coreReq.Id)
							assert.Equal(t, cutil.GetPtr(""), coreReq.Description)
							assert.Equal(t, existing.DeviceType, coreReq.DeviceType)
							assert.Equal(t, model.CoreSkuSchemaVersion, coreReq.SchemaVersion)
							assert.True(t, proto.Equal(components.ToProto(), coreReq.Components))
						}

						after, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)
						if tc.status != http.StatusOK {
							assert.Contains(t, rec.Body.String(), tc.errorMessage)
							assert.Equal(t, before, after, "failed legacy update must not change the REST projection")
							return
						}

						var response model.APISku
						require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
						assert.Equal(t, "", response.Description)
						assert.Equal(t, existing.DeviceType, response.DeviceType)
						assert.Equal(t, []string{"machine-1"}, response.AssociatedMachineIds)
						assert.Equal(t, "", after.Description)
						assert.Equal(t, existing.DeviceType, after.DeviceType)
						assert.Equal(t, []string{"machine-1"}, after.AssociatedMachineIds)
						require.NotNil(t, after.Components)
						assert.True(t, proto.Equal(components.ToProto(), after.Components.SkuComponents))
					})
				}
			},
		},
		{
			name: "uses legacy replacement when another Site Agent rejects dispatch after a successful capability probe",
			run: func(t *testing.T) {
				cases := []struct {
					name         string
					replaceError error
					status       int
				}{
					{name: "legacy replacement succeeds", status: http.StatusOK},
					{
						name: "legacy replacement is independently denied",
						replaceError: skuProxyActivityError("InvokeCoreGRPCOnSite",
							swe.WrapErr(status.Error(codes.PermissionDenied, "legacy replacement denied"))),
						status: http.StatusForbidden,
					},
				}
				for _, tc := range cases {
					t.Run(tc.name, func(t *testing.T) {
						existing := existingSkuProto()
						components := validSkuCreateRequest("").Components
						deviceType := "cpu-server"
						replacementResponse := proto.Clone(existing).(*corev1.Sku)
						replacementResponse.DeviceType = &deviceType
						replacementResponse.Components = components.ToProto()
						replacementResponse.SchemaVersion = model.CoreSkuSchemaVersion
						// Version ran on an upgraded Agent, but the mutation reached an
						// older Agent whose descriptor cannot dispatch the new method.
						dispatchError := skuProxyActivityError("InvokeCoreGRPCOnSite", fmt.Errorf("%w: %q on %q",
							errors.New("unknown proxied gRPC method"), "ReplaceSkuComponents", "forge.Forge"))
						fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
							findResponse:           &corev1.SkuList{Skus: []*corev1.Sku{existing}},
							replaceComponentsError: dispatchError,
							replaceResponse:        replacementResponse,
							replaceError:           tc.replaceError,
						})
						skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
						before, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)

						rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
							DeviceType: &deviceType,
							Components: components,
						}, fixture.updateHandler.Handle)

						require.Equal(t, tc.status, rec.Code, rec.Body.String())
						methods := []string{
							corev1.Forge_Version_FullMethodName,
							corev1.Forge_ReplaceSkuComponents_FullMethodName,
							corev1.Forge_FindSkusByIds_FullMethodName,
							corev1.Forge_ReplaceSku_FullMethodName,
						}
						require.Len(t, fixture.requests, len(methods))
						for i, method := range methods {
							assert.Equal(t, method, fixture.requests[i].FullMethod)
						}
						var replacement corev1.Sku
						require.NoError(t, protojson.Unmarshal(fixture.requests[3].RequestJSON, &replacement))
						assert.Equal(t, existing.Description, replacement.Description)
						assert.Equal(t, &deviceType, replacement.DeviceType)
						assert.True(t, proto.Equal(components.ToProto(), replacement.Components))

						after, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)
						if tc.status != http.StatusOK {
							assert.Contains(t, rec.Body.String(), "legacy replacement denied")
							assert.Equal(t, before, after, "denied legacy replacement must not change the REST projection")
							return
						}

						var response model.APISku
						require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
						assert.Equal(t, existing.GetDescription(), response.Description)
						assert.Equal(t, &deviceType, response.DeviceType)
						require.NotNil(t, response.Components)
						assert.True(t, proto.Equal(replacementResponse.Components, response.Components.ToProto()))
						assert.Equal(t, existing.GetDescription(), after.Description)
						assert.Equal(t, &deviceType, after.DeviceType)
						require.NotNil(t, after.Components)
						assert.True(t, proto.Equal(replacementResponse.Components, after.Components.SkuComponents))
					})
				}
			},
		},
		{
			name: "shares one RPC deadline and preserves the caller context for DB writes",
			run: func(t *testing.T) {
				cases := []struct {
					name                string
					versionResponseJSON []byte
					request             model.APISkuUpdateRequest
					methods             []string
					callerTimeout       time.Duration
				}{
					{
						name:    "component replacement",
						request: model.APISkuUpdateRequest{Components: validSkuCreateRequest("").Components},
						methods: []string{corev1.Forge_Version_FullMethodName, corev1.Forge_ReplaceSkuComponents_FullMethodName},
					},
					{
						name:                "legacy replacement",
						versionResponseJSON: []byte(`{}`),
						request:             model.APISkuUpdateRequest{Components: validSkuCreateRequest("").Components},
						methods:             []string{corev1.Forge_Version_FullMethodName, corev1.Forge_FindSkusByIds_FullMethodName, corev1.Forge_ReplaceSku_FullMethodName},
					},
					{
						name:    "metadata update",
						request: model.APISkuUpdateRequest{Description: cutil.GetPtr("updated description")},
						methods: []string{corev1.Forge_FindSkusByIds_FullMethodName, corev1.Forge_UpdateSkuMetadata_FullMethodName},
					},
					{
						name:                "shorter caller deadline",
						versionResponseJSON: []byte(`{}`),
						request:             model.APISkuUpdateRequest{Components: validSkuCreateRequest("").Components},
						methods:             []string{corev1.Forge_Version_FullMethodName, corev1.Forge_FindSkusByIds_FullMethodName, corev1.Forge_ReplaceSku_FullMethodName},
						callerTimeout:       30 * time.Second,
					},
				}
				for _, tc := range cases {
					t.Run(tc.name, func(t *testing.T) {
						mutationComplete := false
						fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
							versionResponseJSON: tc.versionResponseJSON,
							afterWorkflowGet: func(method string) {
								if method == tc.methods[len(tc.methods)-1] {
									mutationComplete = true
								}
							},
						})
						ctx := context.Background()
						var callerDeadline time.Time
						if tc.callerTimeout != 0 {
							var cancel context.CancelFunc
							ctx, cancel = context.WithTimeout(ctx, tc.callerTimeout)
							defer cancel()
							callerDeadline, _ = ctx.Deadline()
						}

						// Observe actual SKU reads and writes after Core returns. Transaction
						// setup uses a separate context and is not part of the projection.
						var projectionContexts []context.Context
						projectionWrites := 0
						fixture.updateHandler.dbSession.DB.AddQueryHook(skuQueryContextHook(func(ctx context.Context, event *bun.QueryEvent) {
							if !mutationComplete || event.IQuery == nil || event.IQuery.GetTableName() != "sku" {
								return
							}
							switch event.Operation() {
							case "SELECT":
								projectionContexts = append(projectionContexts, ctx)
							case "UPDATE":
								projectionContexts = append(projectionContexts, ctx)
								projectionWrites++
							}
						}))

						rec := fixture.request(t, http.MethodPatch, "sku-1", tc.request, func(c echo.Context) error {
							c.SetRequest(c.Request().WithContext(ctx))
							return fixture.updateHandler.Handle(c)
						})

						require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
						require.Len(t, fixture.requests, len(tc.methods))
						require.Len(t, fixture.startContexts, len(tc.methods))
						require.Len(t, fixture.getContexts, len(tc.methods))
						deadline, ok := fixture.startContexts[0].Deadline()
						require.True(t, ok)
						remaining := time.Until(deadline)
						assert.Positive(t, remaining)
						assert.LessOrEqual(t, remaining, cutil.WorkflowContextTimeout)
						if !callerDeadline.IsZero() {
							assert.True(t, callerDeadline.Equal(deadline), "RPCs must respect the shorter caller deadline")
						}
						for i, method := range tc.methods {
							assert.Equal(t, method, fixture.requests[i].FullMethod)
							startDeadline, hasStartDeadline := fixture.startContexts[i].Deadline()
							getDeadline, hasGetDeadline := fixture.getContexts[i].Deadline()
							require.True(t, hasStartDeadline)
							require.True(t, hasGetDeadline)
							assert.True(t, deadline.Equal(startDeadline), "workflow %d must share the first RPC deadline", i)
							assert.True(t, deadline.Equal(getDeadline), "workflow wait %d must share the first RPC deadline", i)
						}
						require.NotEmpty(t, projectionContexts)
						require.Positive(t, projectionWrites)
						for _, projectionCtx := range projectionContexts {
							projectionDeadline, hasDeadline := projectionCtx.Deadline()
							assert.Equal(t, !callerDeadline.IsZero(), hasDeadline, "projection must use the caller's deadline")
							assert.True(t, callerDeadline.Equal(projectionDeadline), "projection must not inherit the RPC deadline")
							assert.NoError(t, projectionCtx.Err(), "ending the RPC wait must not cancel projection queries")
						}
					})
				}
			},
		},
		{
			name: "does not dispatch another RPC after caller cancellation",
			run: func(t *testing.T) {
				cases := []struct {
					name                   string
					versionResponseJSON    []byte
					replaceComponentsError error
					cancelAfter            string
					methods                []string
					message                string
				}{
					{
						name:        "while waiting for capability discovery",
						cancelAfter: corev1.Forge_Version_FullMethodName,
						methods:     []string{corev1.Forge_Version_FullMethodName},
						message:     "SKU update timed out before Core could apply it",
					},
					{
						name: "while waiting for component replacement",
						replaceComponentsError: skuProxyActivityError("InvokeCoreGRPCOnSite", fmt.Errorf("%w: %q on %q",
							errors.New("unknown proxied gRPC method"), "ReplaceSkuComponents", "forge.Forge")),
						cancelAfter: corev1.Forge_ReplaceSkuComponents_FullMethodName,
						methods:     []string{corev1.Forge_Version_FullMethodName, corev1.Forge_ReplaceSkuComponents_FullMethodName},
						message:     "Core proxy request timed out",
					},
					{
						name:                "while waiting for the legacy read",
						versionResponseJSON: []byte(`{}`),
						cancelAfter:         corev1.Forge_FindSkusByIds_FullMethodName,
						methods:             []string{corev1.Forge_Version_FullMethodName, corev1.Forge_FindSkusByIds_FullMethodName},
						message:             "SKU update timed out before Core could apply it",
					},
				}
				for _, tc := range cases {
					t.Run(tc.name, func(t *testing.T) {
						ctx, cancel := context.WithCancel(context.Background())
						defer cancel()
						fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
							versionResponseJSON:    tc.versionResponseJSON,
							replaceComponentsError: tc.replaceComponentsError,
							afterWorkflowGet: func(method string) {
								if method == tc.cancelAfter {
									cancel()
								}
							},
						})
						skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
						before, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)

						rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
							Description: cutil.GetPtr("updated description"),
							Components:  validSkuCreateRequest(fixture.siteID).Components,
						}, func(c echo.Context) error {
							c.SetRequest(c.Request().WithContext(ctx))
							return fixture.updateHandler.Handle(c)
						})

						require.Equal(t, http.StatusGatewayTimeout, rec.Code, rec.Body.String())
						var response cutil.APIError
						require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
						assert.Equal(t, tc.message, response.Message)
						assert.Nil(t, response.Data)
						require.ErrorIs(t, ctx.Err(), context.Canceled)
						require.Len(t, fixture.requests, len(tc.methods))
						for i, method := range tc.methods {
							assert.Equal(t, method, fixture.requests[i].FullMethod)
						}
						after, err := skuDAO.Get(context.Background(), nil, "sku-1")
						require.NoError(t, err)
						assert.Equal(t, before, after, "cancelled update must not change the REST projection")
					})
				}
			},
		},
		{
			name: "replaces components for SKU with associated machines",
			run: func(t *testing.T) {
				existing := existingSkuProto()
				existing.SchemaVersion = model.CoreSkuSchemaVersion
				replacementResponse := proto.Clone(existing).(*corev1.Sku)
				replacementResponse.Description = cutil.GetPtr("new Core description")
				replacementResponse.DeviceType = cutil.GetPtr("new Core device type")
				replacementResponse.AssociatedMachineIds = []*corev1.MachineId{{Id: "machine-1"}}
				components := validSkuCreateRequest("").Components
				replacementResponse.Components = components.ToProto()
				replacementResponse.Components.Chassis.Model = "Core chassis model"
				fixture := newSkuManagementFixtureWithOptions(t, []string{authz.ProviderAdminRole}, skuManagementFixtureOptions{
					replaceResponse: replacementResponse,
				})
				skuDAO := cdbm.NewSkuDAO(fixture.updateHandler.dbSession)
				_, err := skuDAO.Update(context.Background(), nil, cdbm.SkuUpdateInput{
					SkuID:       "sku-1",
					Description: cutil.GetPtr("REST description"),
					DeviceType:  cutil.GetPtr("REST device type"),
				})
				require.NoError(t, err)

				rec := fixture.request(t, http.MethodPatch, "sku-1", model.APISkuUpdateRequest{
					Components: components,
				}, fixture.updateHandler.Handle)

				require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
				require.Len(t, fixture.requests, 2)
				assert.Equal(t, corev1.Forge_Version_FullMethodName, fixture.requests[0].FullMethod)
				assert.Equal(t, corev1.Forge_ReplaceSkuComponents_FullMethodName, fixture.requests[1].FullMethod)

				var coreReq corev1.ReplaceSkuComponentsRequest
				require.NoError(t, protojson.Unmarshal(fixture.requests[1].RequestJSON, &coreReq))
				assert.Nil(t, coreReq.Description)
				assert.Nil(t, coreReq.DeviceType)
				assert.True(t, proto.Equal(components.ToProto(), coreReq.Components))
				assert.False(t, proto.Equal(coreReq.Components, replacementResponse.Components))

				var response model.APISku
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, []string{"machine-1"}, response.AssociatedMachineIds)
				assert.Equal(t, replacementResponse.GetDescription(), response.Description)
				assert.Equal(t, replacementResponse.DeviceType, response.DeviceType)
				require.NotNil(t, response.Components)
				assert.True(t, proto.Equal(replacementResponse.Components, response.Components.ToProto()))

				saved, err := skuDAO.Get(context.Background(), nil, "sku-1")
				require.NoError(t, err)
				assert.Equal(t, []string{"machine-1"}, saved.AssociatedMachineIds)
				assert.Equal(t, replacementResponse.GetDescription(), saved.Description)
				assert.Equal(t, replacementResponse.DeviceType, saved.DeviceType)
				require.NotNil(t, saved.Components)
				assert.True(t, proto.Equal(replacementResponse.Components, saved.Components.SkuComponents))

				getHandler := NewGetSkuHandler(fixture.updateHandler.dbSession, nil, &config.Config{})
				getRec := fixture.request(t, http.MethodGet, "sku-1", nil, getHandler.Handle)
				require.Equal(t, http.StatusOK, getRec.Code, getRec.Body.String())
				var getResponse model.APISku
				require.NoError(t, json.Unmarshal(getRec.Body.Bytes(), &getResponse))
				assert.Equal(t, []string{"machine-1"}, getResponse.AssociatedMachineIds)
			},
		},
		{
			name: "rejects inverted storage size range",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "rejects non-empty read-only ethernet devices",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "returns Core not found without completing update",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "returns not found for SKU not present in REST database",
			run: func(t *testing.T) {
				fixture := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
				description := "updated description"

				rec := fixture.request(t, http.MethodPatch, "missing-sku", model.APISkuUpdateRequest{
					Description: &description,
				}, fixture.updateHandler.Handle)
				require.Equal(t, http.StatusNotFound, rec.Code, rec.Body.String())
				assert.JSONEq(t, `{"source":"","message":"Could not find SKU with the specified ID","data":null}`, rec.Body.String())
				assert.Empty(t, fixture.requests)
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, tc.run)
	}
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

// skuQueryContextHook observes the context used for SKU projection queries.
type skuQueryContextHook func(context.Context, *bun.QueryEvent)

func (h skuQueryContextHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	h(ctx, event)
	return ctx
}

func (skuQueryContextHook) AfterQuery(context.Context, *bun.QueryEvent) {}

type skuManagementFixture struct {
	org              string
	siteID           string
	user             *cdbm.User
	createHandler    CreateSkuHandler
	updateHandler    UpdateSkuHandler
	deleteHandler    DeleteSkuHandler
	requests         []grpcproxy.Request
	startContexts    []context.Context
	getContexts      []context.Context
	afterWorkflowGet func(string)
}

type skuManagementFixtureOptions struct {
	versionResponseJSON    []byte
	versionError           error
	findResponse           *corev1.SkuList
	findError              error
	createError            error
	replaceResponse        *corev1.Sku
	replaceError           error
	replaceComponentsError error
	updateMetadataError    error
	deleteError            error
	skipPersistedSKU       bool
	afterUpdateMetadata    func(*cdb.Session)
	afterWorkflowGet       func(string)
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

	fixture := &skuManagementFixture{org: org, siteID: site.ID.String(), user: user, afterWorkflowGet: options.afterWorkflowGet}
	client := &tmocks.Client{}
	existing := existingSkuProto()
	if options.versionError != nil {
		fixture.addWorkflowError(client, corev1.Forge_Version_FullMethodName, options.versionError)
	} else if options.versionResponseJSON != nil {
		var response structpb.Struct
		require.NoError(t, protojson.Unmarshal(options.versionResponseJSON, &response))
		fixture.addWorkflow(t, client, corev1.Forge_Version_FullMethodName, &response)
	} else {
		fixture.addWorkflow(t, client, corev1.Forge_Version_FullMethodName, &corev1.BuildInfo{
			Capabilities: []corev1.BuildCapability{corev1.BuildCapability_BUILD_CAPABILITY_REPLACE_SKU_COMPONENTS},
		})
	}
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
	if options.replaceComponentsError != nil {
		fixture.addWorkflowError(client, corev1.Forge_ReplaceSkuComponents_FullMethodName, options.replaceComponentsError)
	} else {
		fixture.addWorkflow(t, client, corev1.Forge_ReplaceSkuComponents_FullMethodName, replaceResponse)
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
	run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		f.getContexts = append(f.getContexts, args.Get(0).(context.Context))
		if f.afterWorkflowGet != nil {
			f.afterWorkflowGet(method)
		}
	}).Return(getErr)
	client.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool { return req.FullMethod == method }),
	).Run(func(args mock.Arguments) {
		f.requests = append(f.requests, args.Get(3).(grpcproxy.Request))
		f.startContexts = append(f.startContexts, args.Get(0).(context.Context))
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
		f.getContexts = append(f.getContexts, args.Get(0).(context.Context))
		out, ok := args.Get(1).(*grpcproxy.Response)
		require.True(t, ok)
		out.ResponseJSON = responseJSON
		for _, callback := range afterGet {
			callback()
		}
		if f.afterWorkflowGet != nil {
			f.afterWorkflowGet(method)
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
		f.startContexts = append(f.startContexts, args.Get(0).(context.Context))
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
