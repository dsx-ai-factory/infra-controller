// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util/labels"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

const derivedTestProfile = "GB200_NVL72R1_C2G4_WiWynn_NVIDIA_WiWynn"

type readbackCancelKey struct{}

type expectedRackDBContextHook struct {
	t       *testing.T
	updates *atomic.Int64
}

func (h expectedRackDBContextHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	_, hasDeadline := ctx.Deadline()
	assert.False(h.t, hasDeadline, "database operations must not inherit the Core workflow deadline")
	return ctx
}

func (h expectedRackDBContextHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if h.updates != nil && strings.Contains(event.Query, `UPDATE "expected_rack"`) {
		h.updates.Add(1)
	}
}

func mockExpectedRackReadback(t *testing.T, client *tmocks.Client, mutation *mock.Call, bulk bool) {
	t.Helper()
	var racks []*corev1.ExpectedRack
	var mutationCtx context.Context
	mutation.Run(func(args mock.Arguments) {
		mutationCtx = args.Get(0).(context.Context)
		if bulk {
			racks = args.Get(3).(*corev1.ExpectedRackList).ExpectedRacks
		} else {
			racks = []*corev1.ExpectedRack{args.Get(3).(*corev1.ExpectedRack)}
		}
		for _, rack := range racks {
			require.Empty(t, rack.GetRackProfileId().GetId(), "caller profile must not reach Core")
		}
		if cancel, ok := mutationCtx.Value(readbackCancelKey{}).(context.CancelFunc); ok {
			cancel()
		}
	})
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "InvokeCoreGRPC", mock.Anything).Return(func(ctx context.Context, _ tclient.StartWorkflowOptions, _ interface{}, args ...interface{}) tclient.WorkflowRun {
		deadline, ok := mutationCtx.Deadline()
		require.True(t, ok)
		readDeadline, ok := ctx.Deadline()
		require.True(t, ok)
		require.Equal(t, deadline, readDeadline, "readback must share the mutation budget")
		request := args[0].(grpcproxy.Request)
		result := &corev1.ExpectedRackList{}
		for _, rack := range racks {
			stored := proto.Clone(rack).(*corev1.ExpectedRack)
			stored.RackProfileId = &corev1.RackProfileId{Id: derivedTestProfile}
			if bulk {
				stored.RackProfileId.Id += "_" + stored.GetRackId().GetId()
			}
			if stored.GetRackId().GetId() == "readback-empty-profile" {
				stored.RackProfileId = nil
			}
			result.ExpectedRacks = append(result.ExpectedRacks, stored)
		}
		var message proto.Message = result
		if bulk {
			slices.Reverse(result.ExpectedRacks)
			require.Equal(t, corev1.Forge_GetAllExpectedRacks_FullMethodName, request.FullMethod)
		} else {
			require.Equal(t, corev1.Forge_GetExpectedRack_FullMethodName, request.FullMethod)
			var lookup corev1.ExpectedRackRequest
			require.NoError(t, protojson.Unmarshal(request.RequestJSON, &lookup))
			require.Equal(t, racks[0].GetRackId().GetId(), lookup.RackId)
			message = result.ExpectedRacks[0]
		}
		encoded, err := protojson.Marshal(message)
		require.NoError(t, err)
		run := &tmocks.WorkflowRun{}
		run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
			args.Get(1).(*grpcproxy.Response).ResponseJSON = encoded
		}).Return(nil)
		return run
	}, nil)
}

// testExpectedRackInitDB initializes a test database session
func testExpectedRackInitDB(t *testing.T) *cdb.Session {
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
	err = dbSession.DB.ResetModel(ctx, (*cdbm.ExpectedRack)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(ctx, (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)

	return dbSession
}

// testExpectedRackSetupTestData creates test infrastructure provider, site, and tenant
func testExpectedRackSetupTestData(t *testing.T, dbSession *cdb.Session, org string) (*cdbm.InfrastructureProvider, *cdbm.Site, *cdbm.Tenant) {
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

	tenant := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: "test-tenant",
		Org:  org,
	}
	_, err = dbSession.DB.NewInsert().Model(tenant).Exec(ctx)
	assert.Nil(t, err)

	return ip, site, tenant
}

func TestCreateExpectedRackHandler_Handle(t *testing.T) {
	e := echo.New()

	dbSession := testExpectedRackInitDB(t)
	dbSession.DB.AddQueryHook(expectedRackDBContextHook{t: t})
	defer dbSession.Close()

	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK in ExpectedRack.CreatedBy
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Create an existing ExpectedRack with a specific (SiteID, RackID) tuple for duplicate test
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	existingRackID := "existing-rack-001"
	_, err = erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         existingRackID,
		SiteID:         site.ID,
		RackProfileID:  "profile-existing",
		CreatedBy:      dbUser.ID,
		Labels: map[string]string{
			"env": "existing",
		},
	})
	assert.Nil(t, err)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mutation := mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedRack", mock.Anything).Return(mockWorkflowRun, nil)
	mockExpectedRackReadback(t, mockTemporalClient, mutation, false)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewCreateExpectedRackHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
				},
			},
		}
	}

	tests := []struct {
		name           string
		requestBody    model.APIExpectedRackCreateRequest
		setupContext   func(c echo.Context)
		expectedStatus int
	}{
		{
			name: "successful creation",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        "test-rack-001",
				RackProfileID: "profile-001",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusCreated,
		},
		{
			name: "successful creation with name, description, and labels",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        "test-rack-002",
				RackProfileID: "profile-002",
				Name:          cutil.GetPtr("Rack 2"),
				Description:   cutil.GetPtr("Second test rack"),
				Labels: map[string]string{
					labels.RackLabelChassisManufacturer: "Acme",
					labels.RackLabelChassisSerialNumber: "SN-12345",
					labels.RackLabelChassisModel:        "RACK-X1000",
					labels.RackLabelLocationRegion:      "us-west",
					labels.RackLabelLocationDatacenter:  "dc-01",
				},
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
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        "test-rack-003",
				RackProfileID: "profile-003",
			},
			setupContext: func(c echo.Context) {
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name: "invalid empty rack_id",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        "",
				RackProfileID: "profile-004",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "invalid whitespace-only rack_id",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        "   ",
				RackProfileID: "profile-005",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "profile omitted and derived by Core",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        "test-rack-006",
				RackProfileID: "",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusCreated,
		},
		{
			name:        "missing Core profile rolls back local create",
			requestBody: model.APIExpectedRackCreateRequest{SiteID: site.ID.String(), RackID: "readback-empty-profile"},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadGateway,
		},
		{
			name:        "expired mutation budget does not start readback",
			requestBody: model.APIExpectedRackCreateRequest{SiteID: site.ID.String(), RackID: "readback-starved"},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
				ctx, cancel := context.WithCancel(c.Request().Context())
				t.Cleanup(cancel)
				c.SetRequest(c.Request().WithContext(context.WithValue(ctx, readbackCancelKey{}, cancel)))
			},
			expectedStatus: http.StatusGatewayTimeout,
		},
		{
			name: "invalid siteId UUID",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        "not-a-uuid",
				RackID:        "test-rack-007",
				RackProfileID: "profile-007",
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
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        "12345678-1234-1234-1234-123456789099",
				RackID:        "test-rack-008",
				RackProfileID: "profile-008",
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
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        unmanagedSite.ID.String(),
				RackID:        "test-rack-009",
				RackProfileID: "profile-009",
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusForbidden,
		},
		{
			name: "duplicate (siteId, rackId) tuple should return 409",
			requestBody: model.APIExpectedRackCreateRequest{
				SiteID:        site.ID.String(),
				RackID:        existingRackID,
				RackProfileID: "profile-duplicate",
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
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			reqBody, _ := json.Marshal(tt.requestBody)
			req := httptest.NewRequest(http.MethodPost, "/v2/org/test-org/expected-rack", bytes.NewReader(reqBody))
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
			if tt.requestBody.RackID == "readback-empty-profile" || tt.requestBody.RackID == "readback-starved" {
				count, err := dbSession.DB.NewSelect().Model((*cdbm.ExpectedRack)(nil)).Where("rack_id = ?", tt.requestBody.RackID).Count(context.Background())
				require.NoError(t, err)
				assert.Zero(t, count)
			}

			if tt.expectedStatus == http.StatusCreated {
				var response model.APIExpectedRack
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				// Server-generated UUID should be present
				assert.NotEqual(t, uuid.Nil, response.ID, "Response should include a server-generated ID UUID")
				assert.Equal(t, tt.requestBody.RackID, response.RackID, "RackID should round-trip")
				assert.Equal(t, derivedTestProfile, response.RackProfileID)
				stored, err := erDAO.Get(ctx, nil, response.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, derivedTestProfile, stored.RackProfileID)
				if tt.requestBody.Name != nil {
					assert.Equal(t, *tt.requestBody.Name, response.Name, "Name in response should match request")
				}
				if tt.requestBody.Description != nil {
					assert.Equal(t, *tt.requestBody.Description, response.Description, "Description in response should match request")
				}
				if tt.requestBody.Labels != nil {
					assert.NotNil(t, response.Labels, "Labels should not be nil in response")
					assert.Equal(t, tt.requestBody.Labels, map[string]string(response.Labels), "Labels in response should match request")
				}
			}
		})
	}
}

func TestGetAllExpectedRackHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := &config.Config{}
	handler := NewGetAllExpectedRackHandler(dbSession, cfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Create expected racks - multiple on managed site, one on unmanaged site
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	managedER1, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "managed-rack-001",
		SiteID:         site.ID,
		RackProfileID:  "profile-managed-1",
		CreatedBy:      dbUser.ID,
		Labels:         map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, managedER1)

	managedER2, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "managed-rack-002",
		SiteID:         site.ID,
		RackProfileID:  "profile-managed-2",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, managedER2)

	managedER3, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "managed-rack-003",
		SiteID:         site.ID,
		RackProfileID:  "profile-managed-3",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, managedER3)

	unmanagedER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "unmanaged-rack-001",
		SiteID:         unmanagedSite.ID,
		RackProfileID:  "profile-unmanaged",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedER)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_VIEWER"},
				},
			},
		}
	}

	tests := []struct {
		name                 string
		siteId               string
		includeRelations     []string
		queryParams          []string
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
				var response []model.APIExpectedRack
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, er := range response {
					assert.NotEqual(t, unmanagedER.ID, er.ID, "Unmanaged rack should not be in response")
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
				var response []model.APIExpectedRack
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, er := range response {
					assert.Equal(t, site.ID, er.SiteID, "All results should be from the specified site")
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
				var response []model.APIExpectedRack
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Greater(t, len(response), 0, "Should return at least one expected rack")
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
		{
			name:        "successful GetAll with pagination",
			siteId:      site.ID.String(),
			queryParams: []string{"pageSize=2", "pageNumber=1"},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response []model.APIExpectedRack
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.LessOrEqual(t, len(response), 2, "Page size should limit response to 2 entries")
			},
		},
	}

	_ = infraProv
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/expected-rack"
			params := []string{}
			if tt.siteId != "" {
				params = append(params, "siteId="+tt.siteId)
			}
			for _, relation := range tt.includeRelations {
				params = append(params, "includeRelation="+relation)
			}
			params = append(params, tt.queryParams...)
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

func TestGetExpectedRackHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()

	cfg := &config.Config{}
	handler := NewGetExpectedRackHandler(dbSession, cfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Create a test ExpectedRack on managed site with rich label set
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	rackLabels := map[string]string{
		"env":                               "test",
		labels.RackLabelChassisManufacturer: "Acme",
		labels.RackLabelChassisSerialNumber: "SN-99999",
		labels.RackLabelChassisModel:        "RACK-X2000",
		labels.RackLabelLocationRegion:      "us-east",
		labels.RackLabelLocationDatacenter:  "dc-02",
	}
	testER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "test-rack-get-001",
		SiteID:         site.ID,
		RackProfileID:  "profile-get-001",
		CreatedBy:      dbUser.ID,
		Name:           "Get Rack",
		Labels:         rackLabels,
	})
	assert.Nil(t, err)
	assert.NotNil(t, testER)

	// Create a test ExpectedRack on unmanaged site
	unmanagedER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "unmanaged-rack-get-001",
		SiteID:         unmanagedSite.ID,
		RackProfileID:  "profile-unmanaged",
		CreatedBy:      dbUser.ID,
		Labels:         map[string]string{"env": "unmanaged"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedER)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
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
			id:   testER.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER.ID.String())
			},
			expectedStatus: http.StatusOK,
		},
		{
			name: "rack not found",
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
			id:   unmanagedER.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedER.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/expected-rack/" + tt.id
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
				var response model.APIExpectedRack
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.Equal(t, testER.ID, response.ID, "ID should match")
				assert.Equal(t, testER.RackID, response.RackID, "RackID should match")
				assert.Equal(t, testER.RackProfileID, response.RackProfileID, "RackProfileID should match")
				assert.Equal(t, testER.Name, response.Name, "Name should match")
				assert.NotNil(t, response.Labels, "Labels should not be nil in response")
				// Verify labels round-trip
				assert.Equal(t, rackLabels, map[string]string(response.Labels), "Labels should round-trip exactly")
				assert.Equal(t, "Acme", response.Labels[labels.RackLabelChassisManufacturer])
				assert.Equal(t, "SN-99999", response.Labels[labels.RackLabelChassisSerialNumber])
				assert.Equal(t, "us-east", response.Labels[labels.RackLabelLocationRegion])
			}
		})
	}
}

func TestUpdateExpectedRackHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Create test ExpectedRacks on managed site
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	testER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "update-rack-001",
		SiteID:         site.ID,
		RackProfileID:  "profile-update-original",
		CreatedBy:      dbUser.ID,
		Labels:         map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, testER)

	testER2, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "update-rack-002",
		SiteID:         site.ID,
		RackProfileID:  "profile-update-original-2",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, testER2)

	// A third ExpectedRack to anchor the rack_id rename-rejection test
	testER3, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "update-rack-003",
		SiteID:         site.ID,
		RackProfileID:  "profile-update-original-3",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, testER3)

	// Create a test ExpectedRack on unmanaged site
	unmanagedER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "update-rack-unmanaged",
		SiteID:         unmanagedSite.ID,
		RackProfileID:  "profile-unmanaged",
		CreatedBy:      dbUser.ID,
		Labels:         map[string]string{"env": "unmanaged"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedER)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "UpdateExpectedRack", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewUpdateExpectedRackHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
				},
			},
		}
	}

	tests := []struct {
		name                 string
		id                   string
		requestBody          model.APIExpectedRackUpdateRequest
		setupContext         func(c echo.Context)
		expectedStatus       int
		checkResponseContent func(t *testing.T, body []byte)
	}{
		{
			name: "legacy profile ignored during metadata update",
			id:   testER.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				RackProfileID: cutil.GetPtr("profile-updated-001"),
				Name:          cutil.GetPtr("updated rack"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER.ID.String())
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response model.APIExpectedRack
				require.NoError(t, json.Unmarshal(body, &response))
				assert.Equal(t, "profile-update-original", response.RackProfileID)
			},
		},
		{
			name: "successful update of name, description, and labels",
			id:   testER2.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				Name:        cutil.GetPtr("Updated Rack"),
				Description: cutil.GetPtr("Updated description"),
				Labels: map[string]string{
					labels.RackLabelChassisManufacturer: "Acme-Updated",
					labels.RackLabelLocationRegion:      "us-central",
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER2.ID.String())
			},
			expectedStatus: http.StatusOK,
		},
		{
			name: "rack_id cannot be changed (immutable)",
			id:   testER3.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				RackID: cutil.GetPtr("update-rack-003-renamed"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER3.ID.String())
			},
			expectedStatus: http.StatusBadRequest,
			checkResponseContent: func(t *testing.T, body []byte) {
				assert.Contains(t, string(body), "RackID cannot be changed after creation")
			},
		},
		{
			name: "rack_id cannot be changed even to an existing value",
			id:   testER3.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				// testER's RackID is already taken in this site, but the rename
				// is rejected as immutable before any duplicate check.
				RackID: cutil.GetPtr("update-rack-001"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER3.ID.String())
			},
			expectedStatus: http.StatusBadRequest,
			checkResponseContent: func(t *testing.T, body []byte) {
				assert.Contains(t, string(body), "RackID cannot be changed after creation")
			},
		},
		{
			name: "identical rack_id remains compatible",
			id:   testER2.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				RackID:        cutil.GetPtr("update-rack-002"),
				RackProfileID: cutil.GetPtr("profile-update-identical-rack-id"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER2.ID.String())
			},
			expectedStatus: http.StatusOK,
			checkResponseContent: func(t *testing.T, body []byte) {
				var response model.APIExpectedRack
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Equal(t, "update-rack-002", response.RackID,
					"reasserting the identity must preserve the stored rackId")
			},
		},
		{
			name: "body ID mismatch with URL should return 400",
			id:   testER.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				ID:            cutil.GetPtr(uuid.New().String()),
				RackProfileID: cutil.GetPtr("profile-should-not-update"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER.ID.String())
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "body ID is not a UUID should return 400",
			id:   testER.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				ID:            cutil.GetPtr("not-a-uuid"),
				RackProfileID: cutil.GetPtr("profile-should-not-update"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER.ID.String())
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "invalid path id (not a UUID) should return 400",
			id:   "not-a-uuid",
			requestBody: model.APIExpectedRackUpdateRequest{
				RackProfileID: cutil.GetPtr("profile-should-not-update"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "not-a-uuid")
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "cannot update on unmanaged site",
			id:   unmanagedER.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				Name: cutil.GetPtr("must-not-update"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedER.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
		{
			name: "site access is checked before rack_id identity",
			id:   unmanagedER.ID.String(),
			requestBody: model.APIExpectedRackUpdateRequest{
				RackID: cutil.GetPtr("update-rack-unmanaged-renamed"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedER.ID.String())
			},
			expectedStatus: http.StatusForbidden,
			checkResponseContent: func(t *testing.T, body []byte) {
				assert.NotContains(t, string(body), "RackID cannot be changed after creation")
			},
		},
		{
			name: "rack not found",
			id:   "12345678-1234-1234-1234-123456789099",
			requestBody: model.APIExpectedRackUpdateRequest{
				Name: cutil.GetPtr("must-not-update"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "12345678-1234-1234-1234-123456789099")
			},
			expectedStatus: http.StatusNotFound,
		},
	}

	_ = infraProv
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			reqBody, _ := json.Marshal(tt.requestBody)
			url := "/v2/org/" + org + "/expected-rack/" + tt.id
			req := httptest.NewRequest(http.MethodPatch, url, bytes.NewReader(reqBody))
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
			if tt.checkResponseContent != nil {
				tt.checkResponseContent(t, rec.Body.Bytes())
			}
		})
	}
}

// TestUpdateExpectedRackHandler_RackIDImmutable verifies that changing an
// ExpectedRack's rackId is rejected before any database mutation or workflow
// trigger, so Cloud, Core, and Flow can never hold different rack IDs for the
// same ExpectedRack. Omitted or identical rackId values remain compatible.
func TestUpdateExpectedRackHandler_RackIDImmutable(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)

	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err := dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	rack, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "immutable-rack-001",
		SiteID:         site.ID,
		RackProfileID:  "profile-original",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, rack)

	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "UpdateExpectedRack", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewUpdateExpectedRackHandler(dbSession, scp, cfg)

	createMockUser := func() *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
				},
			},
		}
	}

	patch := func(body model.APIExpectedRackUpdateRequest) *httptest.ResponseRecorder {
		reqBody, _ := json.Marshal(body)
		req := httptest.NewRequest(http.MethodPatch, "/v2/org/"+org+"/expected-rack/"+rack.ID.String(), bytes.NewReader(reqBody))
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		req = req.WithContext(context.Background())

		rec := httptest.NewRecorder()
		c := e.NewContext(req, rec)
		c.Set("user", createMockUser())
		c.SetParamNames("orgName", "id")
		c.SetParamValues(org, rack.ID.String())

		err := handler.Handle(c)
		assert.Nil(t, err)
		return rec
	}

	getRackID := func() string {
		current, err := erDAO.Get(ctx, nil, rack.ID, nil, false)
		assert.Nil(t, err)
		return current.RackID
	}

	t.Run("changing rack_id is rejected without mutation or workflow", func(t *testing.T) {
		rec := patch(model.APIExpectedRackUpdateRequest{
			RackID:        cutil.GetPtr("immutable-rack-renamed"),
			RackProfileID: cutil.GetPtr("profile-renamed"),
		})

		assert.Equal(t, http.StatusBadRequest, rec.Code)
		assert.Contains(t, rec.Body.String(), "RackID cannot be changed after creation")
		assert.Equal(t, "immutable-rack-001", getRackID())
		mockTemporalClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, "UpdateExpectedRack", mock.Anything)
	})

	t.Run("identical rack_id remains compatible", func(t *testing.T) {
		rec := patch(model.APIExpectedRackUpdateRequest{
			RackID:        cutil.GetPtr("immutable-rack-001"),
			RackProfileID: cutil.GetPtr("profile-identical"),
		})

		assert.Equal(t, http.StatusOK, rec.Code)
		assert.Equal(t, "immutable-rack-001", getRackID())
	})

	t.Run("omitted rack_id remains compatible", func(t *testing.T) {
		rec := patch(model.APIExpectedRackUpdateRequest{
			RackProfileID: cutil.GetPtr("profile-omitted"),
			Name:          cutil.GetPtr("metadata-only-update"),
		})

		assert.Equal(t, http.StatusOK, rec.Code)
		assert.Equal(t, "immutable-rack-001", getRackID())
	})
}

func TestDeleteExpectedRackHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Create a test ExpectedRack on managed site (to be deleted)
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	testER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "delete-rack-001",
		SiteID:         site.ID,
		RackProfileID:  "profile-delete",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, testER)

	// Create a test ExpectedRack on unmanaged site (should not be deletable)
	unmanagedER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		RackID:         "delete-rack-unmanaged",
		SiteID:         unmanagedSite.ID,
		RackProfileID:  "profile-unmanaged-delete",
		CreatedBy:      dbUser.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedER)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteExpectedRack", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewDeleteExpectedRackHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
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
			id:   testER.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testER.ID.String())
			},
			expectedStatus: http.StatusNoContent,
		},
		{
			name: "invalid path id (not a UUID) should return 400",
			id:   "not-a-uuid",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "not-a-uuid")
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "rack not found",
			id:   "12345678-1234-1234-1234-123456789099",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, "12345678-1234-1234-1234-123456789099")
			},
			expectedStatus: http.StatusNotFound,
		},
		{
			name: "cannot delete on unmanaged site",
			id:   unmanagedER.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedER.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/expected-rack/" + tt.id
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

func TestReplaceAllExpectedRacksHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	var profileUpdates atomic.Int64
	dbSession.DB.AddQueryHook(expectedRackDBContextHook{t: t, updates: &profileUpdates})
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mutation := mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "ReplaceAllExpectedRacks", mock.Anything).Return(mockWorkflowRun, nil)
	mockExpectedRackReadback(t, mockTemporalClient, mutation, true)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewReplaceAllExpectedRacksHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
				},
			},
		}
	}

	tests := []struct {
		name             string
		requestBody      model.APIReplaceAllExpectedRacksRequest
		setupBefore      func()
		setupContext     func(c echo.Context)
		expectedStatus   int
		expectedRackIDs  []string
		shouldCheckCount bool
		checkClear       bool
	}{
		{
			name: "successful replace from empty to 3 entries",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: site.ID.String(),
				ExpectedRacks: []*model.APIExpectedRackCreateRequest{
					{
						SiteID:        site.ID.String(),
						RackID:        "replace-rack-001",
						RackProfileID: "profile-replace-001",
						Name:          cutil.GetPtr("Rack one"),
						Description:   cutil.GetPtr("First rack"),
						Labels:        map[string]string{"location.room": "room-a"},
					},
					{
						SiteID:        site.ID.String(),
						RackID:        "replace-rack-002",
						RackProfileID: "profile-replace-002",
						Name:          cutil.GetPtr("Rack two"),
						Labels:        map[string]string{"location.room": "room-b"},
					},
					{
						SiteID:        site.ID.String(),
						RackID:        "replace-rack-003",
						RackProfileID: "profile-replace-003",
					},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus:   http.StatusOK,
			expectedRackIDs:  []string{"replace-rack-001", "replace-rack-002", "replace-rack-003"},
			shouldCheckCount: true,
		},
		{
			name: "successful replace from 3 to 2 entries (scoped to site)",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: site.ID.String(),
				ExpectedRacks: []*model.APIExpectedRackCreateRequest{
					{
						SiteID:        site.ID.String(),
						RackID:        "replace-rack-101",
						RackProfileID: "profile-replace-101",
					},
					{
						SiteID:        site.ID.String(),
						RackID:        "replace-rack-102",
						RackProfileID: "profile-replace-102",
					},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus:   http.StatusOK,
			expectedRackIDs:  []string{"replace-rack-101", "replace-rack-102"},
			shouldCheckCount: true,
		},
		{
			name: "mismatched siteId in entries returns 400",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: site.ID.String(),
				ExpectedRacks: []*model.APIExpectedRackCreateRequest{
					{
						SiteID:        unmanagedSite.ID.String(),
						RackID:        "replace-rack-mismatch",
						RackProfileID: "profile-mismatch",
					},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "duplicate rackId in entries returns 400",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: site.ID.String(),
				ExpectedRacks: []*model.APIExpectedRackCreateRequest{
					{
						SiteID:        site.ID.String(),
						RackID:        "duplicate-rack-id",
						RackProfileID: "profile-dup-1",
					},
					{
						SiteID:        site.ID.String(),
						RackID:        "duplicate-rack-id",
						RackProfileID: "profile-dup-2",
					},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "missing profile rolls back the entire replacement",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: site.ID.String(),
				ExpectedRacks: []*model.APIExpectedRackCreateRequest{
					{SiteID: site.ID.String(), RackID: "valid-replacement"},
					{SiteID: site.ID.String(), RackID: "readback-empty-profile"},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadGateway,
		},
		{
			name: "cannot replace on unmanaged site",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: unmanagedSite.ID.String(),
				ExpectedRacks: []*model.APIExpectedRackCreateRequest{
					{
						SiteID:        unmanagedSite.ID.String(),
						RackID:        "replace-rack-unmanaged",
						RackProfileID: "profile-unmanaged",
					},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusForbidden,
		},
		{
			name: "empty replacement clears only the target site without readback",
			requestBody: model.APIReplaceAllExpectedRacksRequest{
				SiteID: site.ID.String(), ExpectedRacks: []*model.APIExpectedRackCreateRequest{},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusOK, shouldCheckCount: true, checkClear: true,
		},
	}

	_ = infraProv
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			callsBefore := len(mockTemporalClient.Calls)
			profileUpdates.Store(0)
			var otherRack *cdbm.ExpectedRack
			if tt.checkClear {
				for _, targetSite := range []*cdbm.Site{site, unmanagedSite} {
					rack, err := cdbm.NewExpectedRackDAO(dbSession).Create(ctx, nil, cdbm.ExpectedRackCreateInput{
						ExpectedRackID: uuid.New(), SiteID: targetSite.ID, RackID: "clear-test-rack",
						RackProfileID: "legacy-profile", CreatedBy: dbUser.ID,
					})
					require.NoError(t, err)
					if targetSite.ID == unmanagedSite.ID {
						otherRack = rack
					}
				}
			}
			if tt.setupBefore != nil {
				tt.setupBefore()
			}
			var before []cdbm.ExpectedRack
			if tt.expectedStatus == http.StatusBadGateway {
				require.NoError(t, dbSession.DB.NewSelect().Model(&before).Order("id").Scan(ctx))
			}

			reqBody, _ := json.Marshal(tt.requestBody)
			req := httptest.NewRequest(http.MethodPut, "/v2/org/"+org+"/expected-rack", bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			err := handler.Handle(c)

			if tt.expectedStatus == http.StatusOK && len(tt.requestBody.ExpectedRacks) > 0 {
				assert.EqualValues(t, 1, profileUpdates.Load(), "profiles must be written in one batch")
			} else {
				assert.Zero(t, profileUpdates.Load(), "empty or invalid replacements must not write profiles")
			}
			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			if tt.checkClear {
				assert.JSONEq(t, "[]", rec.Body.String())
				count, err := dbSession.DB.NewSelect().Model((*cdbm.ExpectedRack)(nil)).Where("site_id = ?", site.ID).Count(ctx)
				require.NoError(t, err)
				assert.Zero(t, count)
				stored, err := cdbm.NewExpectedRackDAO(dbSession).Get(ctx, nil, otherRack.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, otherRack, stored)
				calls := mockTemporalClient.Calls[callsBefore:]
				require.Len(t, calls, 1, "clearing must invoke only the mutation, not profile readback")
				assert.Equal(t, "ReplaceAllExpectedRacks", calls[0].Arguments.Get(2))
				assert.Empty(t, calls[0].Arguments.Get(3).(*corev1.ExpectedRackList).ExpectedRacks)
			}
			if tt.shouldCheckCount && rec.Code == http.StatusOK {
				var response []model.APIExpectedRack
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.Equal(t, len(tt.expectedRackIDs), len(response), "Response should contain expected number of racks")

				responseIDs := make(map[string]bool)
				for i, er := range response {
					expected := tt.requestBody.ExpectedRacks[i]
					assert.Equal(t, expected.RackID, er.RackID, "response must preserve request order")
					assert.Equal(t, derivedTestProfile+"_"+er.RackID, er.RackProfileID)
					stored, err := cdbm.NewExpectedRackDAO(dbSession).Get(ctx, nil, er.ID, nil, false)
					require.NoError(t, err)
					assert.Equal(t, er.RackProfileID, stored.RackProfileID)
					if expected.Name != nil {
						assert.Equal(t, *expected.Name, stored.Name)
					}
					if expected.Description != nil {
						assert.Equal(t, *expected.Description, stored.Description)
					}
					assert.Equal(t, len(expected.Labels), len(stored.Labels))
					for key, value := range expected.Labels {
						assert.Equal(t, value, stored.Labels[key])
					}
					assert.Equal(t, stored.Name, er.Name)
					assert.Equal(t, stored.Description, er.Description)
					assert.EqualValues(t, stored.Labels, er.Labels)
					responseIDs[er.RackID] = true
					assert.NotEqual(t, uuid.Nil, er.ID, "Each replaced rack should have a server-generated UUID")
				}
				for _, expected := range tt.expectedRackIDs {
					assert.True(t, responseIDs[expected], "Response should contain rack ID %s", expected)
				}
			}
			if tt.expectedStatus == http.StatusBadGateway {
				var after []cdbm.ExpectedRack
				require.NoError(t, dbSession.DB.NewSelect().Model(&after).Order("id").Scan(ctx))
				assert.Equal(t, before, after)
			}
		})
	}
}

func TestDeleteAllExpectedRacksHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site, tenant := testExpectedRackSetupTestData(t, dbSession, org)

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

	// Create a user record for FK
	dbUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test-user"),
	}
	_, err = dbSession.DB.NewInsert().Model(dbUser).Exec(ctx)
	assert.Nil(t, err)

	// Pre-populate some racks on the managed site
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	for i := 0; i < 3; i++ {
		_, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
			ExpectedRackID: uuid.New(),
			RackID:         "delete-all-rack-" + string(rune('a'+i)),
			SiteID:         site.ID,
			RackProfileID:  "profile-delete-all",
			CreatedBy:      dbUser.ID,
		})
		assert.Nil(t, err)
	}

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteAllExpectedRacks", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewDeleteAllExpectedRacksHandler(dbSession, scp, cfg)

	createMockUser := func(org string) *cdbm.User {
		return &cdbm.User{
			ID:          dbUser.ID,
			StarfleetID: cutil.GetPtr("test-user"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       []string{"FORGE_PROVIDER_ADMIN"},
				},
			},
		}
	}

	tests := []struct {
		name           string
		siteID         string
		setupContext   func(c echo.Context)
		expectedStatus int
	}{
		{
			name:   "successful delete with siteId query param",
			siteID: site.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusNoContent,
		},
		{
			name:   "missing siteId query param returns 400",
			siteID: "",
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:   "cannot delete on unmanaged site",
			siteID: unmanagedSite.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv
	_ = tenant

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/expected-rack/all"
			if tt.siteID != "" {
				url += "?siteId=" + tt.siteID
			}
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
