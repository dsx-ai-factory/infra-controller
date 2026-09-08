// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/proto"
)

func testRackInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))

	ctx := context.Background()

	// Reset required tables in dependency order
	err := dbSession.DB.ResetModel(ctx, (*cdbm.User)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.Site)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)

	return dbSession
}

func testRackSetupTestData(t *testing.T, dbSession *cdb.Session, org string) (*cdbm.InfrastructureProvider, *cdbm.Site, *cdbm.Tenant) {
	ctx := context.Background()

	// Create infrastructure provider
	ip := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: "test-provider",
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(ctx)
	assert.Nil(t, err)

	// Create site with Flow enabled
	site := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site",
		Org:                      org,
		InfrastructureProviderID: ip.ID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{Flow: true},
	}
	_, err = dbSession.DB.NewInsert().Model(site).Exec(ctx)
	assert.Nil(t, err)

	// Create tenant with TargetedInstanceCreation enabled (privileged tenant)
	tenant := &cdbm.Tenant{
		ID:        uuid.New(),
		Org:       org,
		CreatedBy: uuid.New(),
	}
	_, err = dbSession.DB.NewInsert().Model(tenant).Exec(ctx)
	assert.Nil(t, err)

	// Create tenant account for privileged tenant
	ta := &cdbm.TenantAccount{
		ID:                       uuid.New(),
		TenantID:                 &tenant.ID,
		InfrastructureProviderID: ip.ID,
		Status:                   cdbm.TenantAccountStatusReady,
		Config:                   cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
	}
	_, err = dbSession.DB.NewInsert().Model(ta).Exec(ctx)
	assert.Nil(t, err)

	return ip, site, tenant
}

func testRackBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	OrgData := cdbm.OrgData{}
	OrgData[org] = cdbm.Org{
		ID:          123,
		Name:        org,
		DisplayName: org,
		OrgType:     "ENTERPRISE",
		Roles:       roles,
	}
	u, err := uDAO.Create(
		context.Background(),
		nil,
		cdbm.UserCreateInput{
			AuxiliaryID: nil,
			StarfleetID: &starfleetID,
			Email:       cutil.GetPtr("test@test.com"),
			FirstName:   cutil.GetPtr("Test"),
			LastName:    cutil.GetPtr("User"),
			OrgData:     OrgData,
		},
	)
	assert.Nil(t, err)

	return u
}

func TestGetRackHandler_Handle(t *testing.T) {
	// Setup
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	// Create a site without Flow enabled
	siteNoRLA := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoRLA).Exec(context.Background())
	assert.Nil(t, err)

	// Create provider user
	providerUser := testRackBuildUser(t, dbSession, "provider-user-rack-get", org, []string{authz.ProviderAdminRole})

	// Create tenant user (no provider role)
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-rack-get", org, []string{authz.TenantAdminRole})

	handler := NewGetRackHandler(dbSession, nil, scp, cfg)

	rackID := "core-rack-01"

	mockRack := &flowv1.Rack{
		ExternalId: rackID,
		Info: &flowv1.DeviceInfo{
			Id:           &flowv1.UUID{Id: uuid.NewString()},
			Name:         "Rack-001",
			Manufacturer: "NVIDIA",
		},
	}

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		rackID         string
		queryParams    map[string]string
		mockRack       *flowv1.Rack
		expectedStatus int
		wantErr        bool
	}{
		{
			name:   "success - get rack by ID",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockRack:       mockRack,
			expectedStatus: http.StatusOK,
			wantErr:        false,
		},
		{
			name:   "failure - Flow not enabled on site",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": siteNoRLA.ID.String(),
			},
			expectedStatus: http.StatusPreconditionFailed,
			wantErr:        true,
		},
		{
			name:        "failure - missing siteId",
			reqOrg:      org,
			user:        providerUser,
			rackID:      rackID,
			queryParams: map[string]string{
				// no siteId
			},
			expectedStatus: http.StatusBadRequest,
			wantErr:        true,
		},
		{
			name:   "failure - invalid siteId",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": uuid.New().String(),
			},
			expectedStatus: http.StatusBadRequest,
			wantErr:        true,
		},
		{
			name:   "failure - rack not found (nil rack)",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockRack:       nil,
			expectedStatus: http.StatusNotFound,
			wantErr:        true,
		},
		{
			name:   "failure - tenant access denied",
			reqOrg: org,
			user:   tenantUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			expectedStatus: http.StatusForbidden,
			wantErr:        true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.GetRackInfoResponse{Rack: tt.mockRack})
			testFlowProxyMethodDispatch(t, mockTemporalClient, mockWorkflowRun, flowv1.Flow_GetRackInfoByID_FullMethodName, func(args mock.Arguments) {
				var request flowv1.GetRackInfoByIDRequest
				testFlowProxyRequest(t, args, &request)
				assert.Equal(t, tt.rackID, request.GetId().GetId())
			})
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/rack/%s?%s", tt.reqOrg, tt.rackID, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.rackID)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("GetRackHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiRack model.APIRack
			err = json.Unmarshal(rec.Body.Bytes(), &apiRack)
			assert.NoError(t, err)
			assert.Equal(t, rackID, apiRack.ID)
		})
	}
}

func TestGetAllRackHandler_Handle(t *testing.T) {
	// Setup
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	// Create a site without Flow enabled
	siteNoRLA := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoRLA).Exec(context.Background())
	assert.Nil(t, err)

	// Create provider user
	providerUser := testRackBuildUser(t, dbSession, "provider-user", org, []string{authz.ProviderAdminRole})

	// Create privileged tenant user
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user", org, []string{authz.TenantAdminRole})

	handler := NewGetAllRackHandler(dbSession, nil, scp, cfg)

	// Helper to create mock Flow response
	createMockRLAResponse := func(racks []*flowv1.Rack, total int32) *flowv1.GetListOfRacksResponse {
		return &flowv1.GetListOfRacksResponse{
			Racks: racks,
			Total: total,
		}
	}

	// Helper to create mock rack
	createMockRack := func(id, name, manufacturer, model string) *flowv1.Rack {
		rackID := &flowv1.UUID{Id: id}
		modelPtr := model
		return &flowv1.Rack{
			Info: &flowv1.DeviceInfo{
				Id:           rackID,
				Name:         name,
				Manufacturer: manufacturer,
				Model:        &modelPtr,
			},
		}
	}

	// Create test racks
	testRacks := []*flowv1.Rack{
		createMockRack("rack-1", "Rack-001", "NVIDIA", "NVL72"),
		createMockRack("rack-2", "Rack-002", "NVIDIA", "NVL72"),
		createMockRack("rack-3", "Rack-003", "Dell", "PowerEdge"),
		createMockRack("rack-4", "Rack-004", "NVIDIA", "NVL36"),
		createMockRack("rack-5", "Rack-005", "Dell", "PowerEdge"),
	}

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		queryParams    map[string]string
		mockResponse   *flowv1.GetListOfRacksResponse
		expectedStatus int
		expectedCount  int
		expectedTotal  *int
		wantErr        bool
	}{
		{
			name:   "success - get all racks",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockResponse:   createMockRLAResponse(testRacks, int32(len(testRacks))),
			expectedStatus: http.StatusOK,
			expectedCount:  len(testRacks),
			expectedTotal:  cutil.GetPtr(len(testRacks)),
			wantErr:        false,
		},
		{
			name:   "success - filter by name",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
				"name":   "Rack-001",
			},
			mockResponse:   createMockRLAResponse([]*flowv1.Rack{testRacks[0]}, 1),
			expectedStatus: http.StatusOK,
			expectedCount:  1,
			expectedTotal:  cutil.GetPtr(1),
			wantErr:        false,
		},
		{
			name:   "success - filter by manufacturer",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":       site.ID.String(),
				"manufacturer": "Dell",
			},
			mockResponse:   createMockRLAResponse([]*flowv1.Rack{testRacks[2], testRacks[4]}, 2),
			expectedStatus: http.StatusOK,
			expectedCount:  2,
			expectedTotal:  cutil.GetPtr(2),
			wantErr:        false,
		},
		{
			name:   "success - filter by name",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
				"name":   "Rack-001",
			},
			mockResponse:   createMockRLAResponse([]*flowv1.Rack{testRacks[0], testRacks[1]}, 2),
			expectedStatus: http.StatusOK,
			expectedCount:  2,
			expectedTotal:  cutil.GetPtr(2),
			wantErr:        false,
		},
		{
			name:   "success - pagination",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":     site.ID.String(),
				"pageNumber": "1",
				"pageSize":   "2",
			},
			mockResponse:   createMockRLAResponse([]*flowv1.Rack{testRacks[0], testRacks[1]}, int32(len(testRacks))),
			expectedStatus: http.StatusOK,
			expectedCount:  2,
			expectedTotal:  cutil.GetPtr(len(testRacks)),
			wantErr:        false,
		},
		{
			name:   "success - orderBy name ASC",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":  site.ID.String(),
				"orderBy": "NAME_ASC",
			},
			mockResponse:   createMockRLAResponse(testRacks, int32(len(testRacks))),
			expectedStatus: http.StatusOK,
			expectedCount:  len(testRacks),
			expectedTotal:  cutil.GetPtr(len(testRacks)),
			wantErr:        false,
		},
		{
			name:   "success - orderBy manufacturer DESC",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":  site.ID.String(),
				"orderBy": "MANUFACTURER_DESC",
			},
			mockResponse:   createMockRLAResponse(testRacks, int32(len(testRacks))),
			expectedStatus: http.StatusOK,
			expectedCount:  len(testRacks),
			expectedTotal:  cutil.GetPtr(len(testRacks)),
			wantErr:        false,
		},
		{
			name:   "failure - Flow not enabled on site",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": siteNoRLA.ID.String(),
			},
			mockResponse:   nil,
			expectedStatus: http.StatusPreconditionFailed,
			wantErr:        true,
		},
		{
			name:   "failure - tenant access denied",
			reqOrg: org,
			user:   tenantUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockResponse:   nil, // No mock response needed for error case
			expectedStatus: http.StatusForbidden,
			wantErr:        true,
		},
		{
			name:   "failure - invalid orderBy",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":  site.ID.String(),
				"orderBy": "INVALID_FIELD_ASC",
			},
			expectedStatus: http.StatusBadRequest,
			wantErr:        true,
		},
		{
			name:   "failure - invalid pagination (negative pageNumber)",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":     site.ID.String(),
				"pageNumber": "-1",
			},
			mockResponse:   nil, // No mock response needed for error case
			expectedStatus: http.StatusBadRequest,
			wantErr:        true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Setup mock Temporal client
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			// Always set up Get mock, even for error cases, as handler may still call it
			if tt.mockResponse != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.GetListOfRacksResponse{
					Racks: tt.mockResponse.Racks,
					Total: tt.mockResponse.Total,
				})
			} else {
				// For error cases, reply with an empty response
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.GetListOfRacksResponse{})
			}
			testFlowProxyDispatch(t, mockTemporalClient, mockWorkflowRun, flowv1.Flow_GetListOfRacks_FullMethodName, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			// Build query string
			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/rack?%s", tt.reqOrg, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)
			// In Echo, c.JSON() returns nil on success, so err can be nil even when returning error response
			// Check status code instead of err for error cases
			if tt.expectedStatus != rec.Code {
				t.Errorf("GetAllRackHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			// Verify response
			var apiRacks []*model.APIRack
			err = json.Unmarshal(rec.Body.Bytes(), &apiRacks)
			assert.NoError(t, err)
			assert.Equal(t, tt.expectedCount, len(apiRacks))

			// Verify pagination header
			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			if tt.expectedTotal != nil {
				assert.Equal(t, *tt.expectedTotal, pr.Total)
			}
		})
	}
}

func TestValidateRackHandler_Handle(t *testing.T) {
	// Setup
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	// Create a site without Flow enabled
	siteNoRLA := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoRLA).Exec(context.Background())
	assert.Nil(t, err)

	// Create provider user
	providerUser := testRackBuildUser(t, dbSession, "provider-user-validate", org, []string{authz.ProviderAdminRole})

	// Create tenant user (should be denied)
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-validate", org, []string{authz.TenantAdminRole})

	handler := NewValidateRackHandler(dbSession, nil, scp, cfg)

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	rackID := uuid.NewString()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		rackID         string
		queryParams    map[string]string
		mockResponse   *flowv1.ValidateComponentsResponse
		expectedStatus int
		wantErr        bool
	}{
		{
			name:   "success - validate rack with no diffs",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockResponse: &flowv1.ValidateComponentsResponse{
				Diffs:           []*flowv1.ComponentDiff{},
				TotalDiffs:      0,
				MissingCount:    0,
				UnexpectedCount: 0,
				MismatchCount:   0,
				MatchCount:      5,
			},
			expectedStatus: http.StatusOK,
			wantErr:        false,
		},
		{
			name:   "success - validate rack with diffs",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockResponse: &flowv1.ValidateComponentsResponse{
				Diffs: []*flowv1.ComponentDiff{
					{
						Type:        flowv1.DiffType_DIFF_TYPE_MISSING,
						ComponentId: "comp-1",
					},
				},
				TotalDiffs:      1,
				MissingCount:    1,
				UnexpectedCount: 0,
				MismatchCount:   0,
				MatchCount:      4,
			},
			expectedStatus: http.StatusOK,
			wantErr:        false,
		},
		{
			name:   "failure - Flow not enabled on site",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": siteNoRLA.ID.String(),
			},
			mockResponse:   nil,
			expectedStatus: http.StatusPreconditionFailed,
			wantErr:        true,
		},
		{
			name:   "failure - tenant user forbidden",
			reqOrg: org,
			user:   tenantUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockResponse:   nil,
			expectedStatus: http.StatusForbidden,
			wantErr:        true,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			queryParams:    map[string]string{},
			mockResponse:   nil,
			expectedStatus: http.StatusBadRequest,
			wantErr:        true,
		},
		{
			name:   "failure - invalid siteId",
			reqOrg: org,
			user:   providerUser,
			rackID: rackID,
			queryParams: map[string]string{
				"siteId": uuid.NewString(), // non-existent site
			},
			mockResponse:   nil,
			expectedStatus: http.StatusBadRequest,
			wantErr:        true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Setup mock Temporal client
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			dispatchedRackID := ""
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockResponse != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.ValidateComponentsResponse{
					Diffs:           tt.mockResponse.Diffs,
					TotalDiffs:      tt.mockResponse.TotalDiffs,
					MissingCount:    tt.mockResponse.MissingCount,
					UnexpectedCount: tt.mockResponse.UnexpectedCount,
					MismatchCount:   tt.mockResponse.MismatchCount,
					MatchCount:      tt.mockResponse.MatchCount,
				})
			} else {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.ValidateComponentsResponse{})
			}
			testFlowProxyMethodDispatch(
				t,
				mockTemporalClient,
				mockWorkflowRun,
				flowv1.Flow_ValidateComponents_FullMethodName,
				func(args mock.Arguments) {
					flowRequest := &flowv1.ValidateComponentsRequest{}
					testFlowProxyRequest(t, args, flowRequest)
					targets := flowRequest.GetTargetSpec().GetRacks().GetTargets()
					require.Len(t, targets, 1)
					dispatchedRackID = targets[0].GetExternalId()
				},
			)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			// Build query string
			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/rack/%s/validation?%s", tt.reqOrg, tt.rackID, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.rackID)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)
			if tt.expectedStatus != rec.Code {
				t.Errorf("ValidateRackHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}
			assert.Equal(t, tt.rackID, dispatchedRackID)

			// Verify response
			var apiResult model.APIRackValidationResult
			err = json.Unmarshal(rec.Body.Bytes(), &apiResult)
			assert.NoError(t, err)
			assert.Equal(t, tt.mockResponse.TotalDiffs, apiResult.TotalDiffs)
			assert.Equal(t, tt.mockResponse.MatchCount, apiResult.MatchCount)
			assert.Equal(t, len(tt.mockResponse.Diffs), len(apiResult.Diffs))
		})
	}
}

func TestValidateRacksHandler_Handle(t *testing.T) {
	// Setup
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	// Create a site without Flow enabled
	siteNoRLA := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoRLA).Exec(context.Background())
	assert.Nil(t, err)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-validate-racks", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-validate-racks", org, []string{authz.TenantAdminRole})

	handler := NewValidateRacksHandler(dbSession, nil, scp, cfg)

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		queryParams    map[string]string
		mockResponse   *flowv1.ValidateComponentsResponse
		expectedStatus int
	}{
		{
			name:   "success - validate all racks (no filter)",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockResponse: &flowv1.ValidateComponentsResponse{
				Diffs:           []*flowv1.ComponentDiff{},
				TotalDiffs:      0,
				MissingCount:    0,
				UnexpectedCount: 0,
				MismatchCount:   0,
				MatchCount:      10,
			},
			expectedStatus: http.StatusOK,
		},
		{
			name:   "success - validate with name filter",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
				"name":   "Rack-001",
			},
			mockResponse: &flowv1.ValidateComponentsResponse{
				Diffs:           []*flowv1.ComponentDiff{},
				TotalDiffs:      0,
				MissingCount:    0,
				UnexpectedCount: 0,
				MismatchCount:   0,
				MatchCount:      5,
			},
			expectedStatus: http.StatusOK,
		},
		{
			name:   "success - validate with manufacturer filter",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":       site.ID.String(),
				"manufacturer": "NVIDIA",
			},
			mockResponse: &flowv1.ValidateComponentsResponse{
				Diffs: []*flowv1.ComponentDiff{
					{
						Type:        flowv1.DiffType_DIFF_TYPE_MISMATCH,
						ComponentId: "comp-1",
						FieldDiffs: []*flowv1.FieldDiff{
							{
								FieldName:     "firmware_version",
								ExpectedValue: "1.0.0",
								ActualValue:   "2.0.0",
							},
						},
					},
				},
				TotalDiffs:      1,
				MissingCount:    0,
				UnexpectedCount: 0,
				MismatchCount:   1,
				MatchCount:      7,
			},
			expectedStatus: http.StatusOK,
		},
		{
			name:   "success - validate with multiple filters",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId":       site.ID.String(),
				"name":         "Rack-001",
				"manufacturer": "NVIDIA",
			},
			mockResponse: &flowv1.ValidateComponentsResponse{
				Diffs:      []*flowv1.ComponentDiff{},
				TotalDiffs: 0,
				MatchCount: 3,
			},
			expectedStatus: http.StatusOK,
		},
		{
			name:   "failure - Flow not enabled on site",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": siteNoRLA.ID.String(),
			},
			expectedStatus: http.StatusPreconditionFailed,
		},
		{
			name:   "failure - tenant user forbidden",
			reqOrg: org,
			user:   tenantUser,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:   "failure - invalid siteId",
			reqOrg: org,
			user:   providerUser,
			queryParams: map[string]string{
				"siteId": uuid.NewString(),
			},
			expectedStatus: http.StatusBadRequest,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockResponse != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.ValidateComponentsResponse{
					Diffs:           tt.mockResponse.Diffs,
					TotalDiffs:      tt.mockResponse.TotalDiffs,
					MissingCount:    tt.mockResponse.MissingCount,
					UnexpectedCount: tt.mockResponse.UnexpectedCount,
					MismatchCount:   tt.mockResponse.MismatchCount,
					MatchCount:      tt.mockResponse.MatchCount,
				})
			} else {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.ValidateComponentsResponse{})
			}
			testFlowProxyDispatch(t, mockTemporalClient, mockWorkflowRun, flowv1.Flow_ValidateComponents_FullMethodName, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/rack/validation?%s", tt.reqOrg, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)
			if tt.expectedStatus != rec.Code {
				t.Errorf("ValidateRacksHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResult model.APIRackValidationResult
			err = json.Unmarshal(rec.Body.Bytes(), &apiResult)
			assert.NoError(t, err)
			assert.Equal(t, tt.mockResponse.TotalDiffs, apiResult.TotalDiffs)
			assert.Equal(t, tt.mockResponse.MatchCount, apiResult.MatchCount)
			assert.Equal(t, len(tt.mockResponse.Diffs), len(apiResult.Diffs))
		})
	}
}

func TestUpdateRackPowerStateHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-pc-rack", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-pc-rack", org, []string{authz.TenantAdminRole})

	handler := NewUpdateRackPowerStateHandler(dbSession, nil, scp, cfg)

	rackID := uuid.New().String()

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		rackID         string
		body           string
		mockTaskIDs    []*flowv1.UUID
		expectedStatus int
	}{
		{
			name:           "success - power on rack",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"on"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - power off rack",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"off"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - power cycle rack",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"cycle"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - force power off rack",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"forceoff"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - force power cycle rack",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"forcecycle"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - invalid state",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"reboot"}`, site.ID.String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - empty state",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":""}`, site.ID.String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           `{"state":"on"}`,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"on"}`, uuid.New().String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"on"}`, site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{TaskIds: tt.mockTaskIDs})
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/%s/power", tt.reqOrg, tt.rackID)

			req := httptest.NewRequest(http.MethodPatch, path, strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.rackID)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("UpdateRackPowerStateHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResp model.APIUpdatePowerStateResponse
			err = json.Unmarshal(rec.Body.Bytes(), &apiResp)
			assert.NoError(t, err)
			assert.NotEmpty(t, apiResp.TaskIDs)
		})
	}
}

func TestBatchUpdateRackPowerStateHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-pc-rack-batch", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-pc-rack-batch", org, []string{authz.TenantAdminRole})

	handler := NewBatchUpdateRackPowerStateHandler(dbSession, nil, scp, cfg)

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		body           string
		mockTaskIDs    []*flowv1.UUID
		expectedStatus int
	}{
		{
			name:           "success - power on all racks (no filter)",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"on"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}, {Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - power off with name filter",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","filter":{"names":["Rack-001"]},"state":"off"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			body:           `{"state":"on"}`,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid state",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"reboot"}`, site.ID.String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"on"}`, site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - invalid siteId",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","state":"on"}`, uuid.New().String()),
			expectedStatus: http.StatusBadRequest,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{TaskIds: tt.mockTaskIDs})
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/power", tt.reqOrg)

			req := httptest.NewRequest(http.MethodPatch, path, strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("BatchUpdateRackPowerStateHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResp model.APIUpdatePowerStateResponse
			err = json.Unmarshal(rec.Body.Bytes(), &apiResp)
			assert.NoError(t, err)
			assert.NotEmpty(t, apiResp.TaskIDs)
		})
	}
}

func TestUpdateRackFirmwareHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-fw-rack", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-fw-rack", org, []string{authz.TenantAdminRole})

	handler := NewUpdateRackFirmwareHandler(dbSession, nil, scp, cfg)

	rackID := uuid.New().String()

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		rackID         string
		body           string
		mockTaskIDs    []*flowv1.UUID
		expectedAuth   string
		expectedStatus int
	}{
		{
			name:           "success - firmware update with authentication data",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","version":"24.11.0","authenticationData":{"shared":"rack-token"}}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedAuth:   "rack-token",
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - firmware update without version",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           `{}`,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, uuid.New().String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{TaskIds: tt.mockTaskIDs})
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).
				Run(func(args mock.Arguments) {
					if tt.expectedAuth == "" {
						return
					}
					flowReq := &flowv1.UpgradeFirmwareRequest{}
					testFlowProxyRequestWithSecrets(t, args, site.ID.String(), tt.expectedAuth, flowReq)
					assert.Equal(t, tt.expectedAuth, flowReq.GetAuthenticationData().GetShared())
				}).
				Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/%s/firmware", tt.reqOrg, tt.rackID)

			req := httptest.NewRequest(http.MethodPatch, path, strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.rackID)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("UpdateRackFirmwareHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResp model.APIUpdateFirmwareResponse
			err = json.Unmarshal(rec.Body.Bytes(), &apiResp)
			assert.NoError(t, err)
			assert.NotEmpty(t, apiResp.TaskIDs)
		})
	}
}

func TestBringUpRackHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-bu-rack", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-bu-rack", org, []string{authz.TenantAdminRole})

	handler := NewBringUpRackHandler(dbSession, nil, scp, cfg)

	rackID := uuid.New().String()

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		rackID         string
		body           string
		mockTaskIDs    []*flowv1.UUID
		expectedStatus int
	}{
		{
			name:           "success - bring up rack",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - bring up rack with description",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s","description":"test bring up"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           `{}`,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid siteId",
			reqOrg:         org,
			user:           providerUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, uuid.New().String()),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			rackID:         rackID,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{TaskIds: tt.mockTaskIDs})
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/%s/bringup", tt.reqOrg, tt.rackID)

			req := httptest.NewRequest(http.MethodPost, path, strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.rackID)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("BringUpRackHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResp model.APIBringUpRackResponse
			err = json.Unmarshal(rec.Body.Bytes(), &apiResp)
			assert.NoError(t, err)
			assert.NotEmpty(t, apiResp.TaskIDs)
		})
	}
}

func TestBatchBringUpRackHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-bu-rack-batch", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-bu-rack-batch", org, []string{authz.TenantAdminRole})

	handler := NewBatchBringUpRackHandler(dbSession, nil, scp, cfg)

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		body           string
		mockTaskIDs    []*flowv1.UUID
		expectedStatus int
	}{
		{
			name:           "success - bring up all racks (no filter)",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}, {Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - bring up with name filter",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","filter":{"names":["Rack-001"]}}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - bring up with description",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","description":"batch bring up test"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			body:           `{}`,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - invalid siteId",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, uuid.New().String()),
			expectedStatus: http.StatusBadRequest,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{TaskIds: tt.mockTaskIDs})
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/bringup", tt.reqOrg)

			req := httptest.NewRequest(http.MethodPost, path, strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("BatchBringUpRackHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResp model.APIBringUpRackResponse
			err = json.Unmarshal(rec.Body.Bytes(), &apiResp)
			assert.NoError(t, err)
			assert.NotEmpty(t, apiResp.TaskIDs)
		})
	}
}

func TestBatchUpdateRackFirmwareHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-fw-rack-batch", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-fw-rack-batch", org, []string{authz.TenantAdminRole})

	handler := NewBatchUpdateRackFirmwareHandler(dbSession, nil, scp, cfg)

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		body           string
		mockTaskIDs    []*flowv1.UUID
		expectedAuth   string
		expectedStatus int
	}{
		{
			name:           "success - firmware update all racks with authentication data",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","authenticationData":{"shared":"batch-rack-token"}}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}, {Id: uuid.NewString()}},
			expectedAuth:   "batch-rack-token",
			expectedStatus: http.StatusOK,
		},
		{
			name:           "success - firmware update with name filter and version",
			reqOrg:         org,
			user:           providerUser,
			body:           fmt.Sprintf(`{"siteId":"%s","filter":{"names":["rack-1"]},"version":"24.11.0"}`, site.ID.String()),
			mockTaskIDs:    []*flowv1.UUID{{Id: uuid.NewString()}},
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			body:           `{}`,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			body:           fmt.Sprintf(`{"siteId":"%s"}`, site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{TaskIds: tt.mockTaskIDs})
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).
				Run(func(args mock.Arguments) {
					if tt.expectedAuth == "" {
						return
					}
					flowReq := &flowv1.UpgradeFirmwareRequest{}
					testFlowProxyRequestWithSecrets(t, args, site.ID.String(), tt.expectedAuth, flowReq)
					assert.Equal(t, tt.expectedAuth, flowReq.GetAuthenticationData().GetShared())
				}).
				Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/firmware", tt.reqOrg)

			req := httptest.NewRequest(http.MethodPatch, path, strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			ec.Set("user", tt.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("BatchUpdateRackFirmwareHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiResp model.APIUpdateFirmwareResponse
			err = json.Unmarshal(rec.Body.Bytes(), &apiResp)
			assert.NoError(t, err)
			assert.NotEmpty(t, apiResp.TaskIDs)
		})
	}
}

// TestRackHandlers_RequestPassThrough asserts the identifiers sent by each
// rack-scoped operation handler through the shared Execute* helpers.
func TestRackHandlers_RequestPassThrough(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org-rule-pass"
	_, site, _ := testRackSetupTestData(t, dbSession, org)
	providerUser := testRackBuildUser(t, dbSession, "provider-user-rule-pass", org, []string{authz.ProviderAdminRole})

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	rackID := uuid.New().String()
	ruleID := uuid.NewString()

	cases := []struct {
		name          string
		path          string
		body          string
		handler       echo.HandlerFunc
		flowReq       proto.Message
		extractRID    func(req proto.Message) string
		extractRackID func(req proto.Message) string
	}{
		{
			name: "power - PowerOnRackRequest carries rule_id",
			path: fmt.Sprintf("/v2/org/%s/nico/rack/%s/power", org, rackID),
			body: fmt.Sprintf(`{"siteId":%q,"state":"on","ruleId":%q}`, site.ID.String(), ruleID),
			handler: func() echo.HandlerFunc {
				return NewUpdateRackPowerStateHandler(dbSession, nil, scp, cfg).Handle
			}(),
			flowReq: &flowv1.PowerOnRackRequest{},
			extractRID: func(req proto.Message) string {
				return req.(*flowv1.PowerOnRackRequest).GetRuleId().GetId()
			},
			extractRackID: func(req proto.Message) string {
				return req.(*flowv1.PowerOnRackRequest).GetTargetSpec().GetRacks().GetTargets()[0].GetExternalId()
			},
		},
		{
			name: "firmware - UpgradeFirmwareRequest carries rule_id",
			path: fmt.Sprintf("/v2/org/%s/nico/rack/%s/firmware", org, rackID),
			body: fmt.Sprintf(`{"siteId":%q,"ruleId":%q}`, site.ID.String(), ruleID),
			handler: func() echo.HandlerFunc {
				return NewUpdateRackFirmwareHandler(dbSession, nil, scp, cfg).Handle
			}(),
			flowReq: &flowv1.UpgradeFirmwareRequest{},
			extractRID: func(req proto.Message) string {
				return req.(*flowv1.UpgradeFirmwareRequest).GetRuleId().GetId()
			},
			extractRackID: func(req proto.Message) string {
				return req.(*flowv1.UpgradeFirmwareRequest).GetTargetSpec().GetRacks().GetTargets()[0].GetExternalId()
			},
		},
		{
			name: "bring-up - BringUpRackRequest carries rule_id",
			path: fmt.Sprintf("/v2/org/%s/nico/rack/%s/bringup", org, rackID),
			body: fmt.Sprintf(`{"siteId":%q,"ruleId":%q}`, site.ID.String(), ruleID),
			handler: func() echo.HandlerFunc {
				return NewBringUpRackHandler(dbSession, nil, scp, cfg).Handle
			}(),
			flowReq: &flowv1.BringUpRackRequest{},
			extractRID: func(req proto.Message) string {
				return req.(*flowv1.BringUpRackRequest).GetRuleId().GetId()
			},
			extractRackID: func(req proto.Message) string {
				return req.(*flowv1.BringUpRackRequest).GetTargetSpec().GetRacks().GetTargets()[0].GetExternalId()
			},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dispatched := false

			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, mockWorkflowRun, &flowv1.SubmitTaskResponse{
				TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
			})
			mockTemporalClient.Mock.On("ExecuteWorkflow",
				mock.Anything, mock.Anything, grpcproxy.Flow.WorkflowName, mock.Anything,
			).Run(func(args mock.Arguments) {
				testFlowProxyRequest(t, args, tc.flowReq)
				dispatched = true
			}).Return(mockWorkflowRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			req := httptest.NewRequest(http.MethodPatch, tc.path, strings.NewReader(tc.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(org, rackID)
			ec.Set("user", providerUser)

			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := tc.handler(ec)
			require.NoError(t, err)
			require.Equal(t, http.StatusOK, rec.Code, "body=%s", rec.Body.String())

			require.True(t, dispatched, "the Flow proxy workflow was not started")
			assert.Equal(t, ruleID, tc.extractRID(tc.flowReq))
			assert.Equal(t, rackID, tc.extractRackID(tc.flowReq))
		})
	}
}
