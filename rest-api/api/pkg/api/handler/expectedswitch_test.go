// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
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
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
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
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// testExpectedSwitchInitDB initializes a test database session
func testExpectedSwitchInitDB(t *testing.T) *cdb.Session {
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
	err = dbSession.DB.ResetModel(ctx, (*cdbm.ExpectedSwitch)(nil))
	assert.Nil(t, err)

	return dbSession
}

// testExpectedSwitchSetupTestData creates test infrastructure provider and site
func testExpectedSwitchSetupTestData(t *testing.T, dbSession *cdb.Session, org string) (*cdbm.InfrastructureProvider, *cdbm.Site) {
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

func TestCreateExpectedSwitchHandler_Handle(t *testing.T) {
	e := echo.New()

	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site := testExpectedSwitchSetupTestData(t, dbSession, org)

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

	// Create an existing Expected Switch with specific BMC and NVOS MAC
	// addresses for the duplicate tests
	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	existingMAC := "AA:BB:CC:DD:EE:11"
	_, err = esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      existingMAC,
		SwitchSerialNumber: "EXISTING-SWITCH-001",
		NvosMacAddresses:   []string{"AA:BB:CC:DD:EE:22"},
		Labels:             map[string]string{"env": "existing"},
	})
	assert.Nil(t, err)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	var capturedCreate *corev1.ExpectedSwitch
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedSwitch", mock.Anything).
		Run(func(args mock.Arguments) {
			capturedCreate = args.Get(3).(*corev1.ExpectedSwitch)
		}).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewCreateExpectedSwitchHandler(dbSession, scp, cfg)

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
		requestBody    model.APIExpectedSwitchCreateRequest
		setupContext   func(c echo.Context)
		expectedStatus int
		expectedError  string
	}{
		{
			name: "successful creation with single NVOS interface address",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "02:00:00:00:20:01",
				SwitchSerialNumber: "NVOS-IP-CREATE",
				NvosInterfaces: model.APINvosInterfaces{{
					MacAddress: "02:00:00:00:21:01",
					IsPrimary:  true,
					IPAddress:  cutil.GetPtr("192.0.2.20"),
				}},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusCreated,
		},
		{
			name: "NVOS address on a multi-port switch is rejected",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "02:00:00:00:20:02",
				SwitchSerialNumber: "NVOS-IP-MULTI",
				NvosInterfaces: model.APINvosInterfaces{
					{MacAddress: "02:00:00:00:21:02"},
					{
						MacAddress: "02:00:00:00:21:03",
						IPAddress:  cutil.GetPtr("192.0.2.20"),
					},
				},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
			expectedError:  "ipAddress requires exactly one nvosInterfaces entry",
		},
		{
			name: "unspecified NVOS address is rejected",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "02:00:00:00:20:03",
				SwitchSerialNumber: "NVOS-IP-UNSPECIFIED",
				NvosInterfaces: model.APINvosInterfaces{{
					MacAddress: "02:00:00:00:21:04",
					IPAddress:  cutil.GetPtr("0.0.0.0"),
				}},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
			expectedError:  "ipAddress must be a valid IPv4 or IPv6 address that is not unspecified, multicast, or limited broadcast",
		},
		{
			name: "malformed NVOS address is rejected",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "02:00:00:00:20:02",
				SwitchSerialNumber: "NVOS-IP-INVALID",
				NvosInterfaces: model.APINvosInterfaces{{
					MacAddress: "02:00:00:00:21:02",
					IPAddress:  cutil.GetPtr("not-an-ip"),
				}},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
			expectedError:  "ipAddress must be a valid IPv4 or IPv6 address",
		},
		{
			name: "successful creation with a 256-character BMC username and 255-character password",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("u", 256)),
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 255)),
				SwitchSerialNumber: "SWITCH123",
				NvOsUsername:       cutil.GetPtr("nvos-admin"),
				NvOsPassword:       cutil.GetPtr("nvos-password"),
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "00:11:22:33:44:66"}, {MacAddress: "00:11:22:33:44:67", IsPrimary: true}},
				BmcIpAddress:       cutil.GetPtr("192.168.1.10"),
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
			name: "creation rejects missing interfaces even with deprecated NvosMacAddresses",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:56",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "SWITCH124",
				NvosMacAddresses:   json.RawMessage(`["AA:BB:CC:DD:EE:22", "invalid", "invalid"]`),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "missing user context",
			requestBody: model.APIExpectedSwitchCreateRequest{
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "02:00:00:00:10:01"}},
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:77",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "SWITCH125",
			},
			setupContext: func(c echo.Context) {
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name: "invalid mac address length",
			requestBody: model.APIExpectedSwitchCreateRequest{
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "02:00:00:00:10:01"}},
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "SWITCH126",
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
			requestBody: model.APIExpectedSwitchCreateRequest{
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "02:00:00:00:10:01"}},
				SiteID:             "12345678-1234-1234-1234-123456789099",
				BmcMacAddress:      "00:11:22:33:44:88",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "SWITCH127",
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
			requestBody: model.APIExpectedSwitchCreateRequest{
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "02:00:00:00:10:01"}},
				SiteID:             unmanagedSite.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:99",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "SWITCH128",
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
			requestBody: model.APIExpectedSwitchCreateRequest{
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "02:00:00:00:10:01"}},
				SiteID:             site.ID.String(),
				BmcMacAddress:      existingMAC,
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "DUPLICATE-SWITCH-999",
				Labels:             map[string]string{"env": "duplicate-test"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName")
				c.SetParamValues(org)
			},
			expectedStatus: http.StatusConflict,
		},
		{
			name: "NVOS MAC claimed by another switch should return 409",
			requestBody: model.APIExpectedSwitchCreateRequest{
				SiteID:             site.ID.String(),
				BmcMacAddress:      "00:11:22:33:44:AA",
				DefaultBmcUsername: cutil.GetPtr("admin"),
				DefaultBmcPassword: cutil.GetPtr("password"),
				SwitchSerialNumber: "NVOS-DUP-SWITCH-001",
				NvosInterfaces:     model.APINvosInterfaces{{MacAddress: "aa-bb-cc-dd-ee-22"}},
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
			req := httptest.NewRequest(http.MethodPost, "/v2/org/test-org/nico/expected-switch", bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)

			workflowCallsBefore := len(mockTemporalClient.Calls)
			before, _, readErr := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{}, paginator.PageInput{}, nil)
			require.NoError(t, readErr)
			err := handler.Handle(c)

			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}
			if tt.expectedError != "" {
				assert.Contains(t, rec.Body.String(), tt.expectedError)
				assert.Len(t, mockTemporalClient.Calls, workflowCallsBefore)
				after, _, readErr := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{}, paginator.PageInput{}, nil)
				require.NoError(t, readErr)
				assert.Equal(t, before, after, "invalid updates must not be saved")
			}

			if tt.expectedStatus == http.StatusCreated {
				require.Len(t, mockTemporalClient.Calls, workflowCallsBefore+1)
				require.NotNil(t, capturedCreate)
				assert.Equal(t, tt.requestBody.NvosInterfaces.IPAddress(), capturedCreate.NvosIpAddress)
				var response model.APIExpectedSwitch
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				if tt.requestBody.Labels != nil {
					assert.NotNil(t, response.Labels, "Labels should not be nil in response")
					assert.Equal(t, tt.requestBody.Labels, map[string]string(response.Labels), "Labels in response should match request")
				}
				if tt.requestBody.BmcIpAddress != nil {
					if assert.NotNil(t, response.BmcIpAddress, "BmcIpAddress should not be nil in response") {
						assert.Equal(t, *tt.requestBody.BmcIpAddress, *response.BmcIpAddress, "BmcIpAddress in response should match request")
					}
				}
				if tt.requestBody.NvosInterfaces != nil || tt.requestBody.NvosMacAddresses != nil {
					expected := tt.requestBody.NvosInterfaces
					if expected.PrimaryMacAddress() == nil && len(expected) > 0 {
						expected[0].IsPrimary = true
					}
					assert.Equal(t, append(model.APINvosInterfaces{}, expected...), response.NvosInterfaces)
					assert.Equal(t, append(model.APINvosMacAddresses{}, expected.MacAddresses()...), response.NvosMacAddresses)
					stored, readErr := esDAO.Get(ctx, nil, response.ID, nil, false)
					require.NoError(t, readErr)
					assert.Equal(t, expected.MacAddresses(), stored.NvosMacAddresses)
					assert.Equal(t, expected.PrimaryMacAddress(), stored.NvosPrimaryMacAddress)
					assert.Equal(t, expected.IPAddress(), stored.NvosIpAddress)
				}
			}
		})
	}
}

func TestGetAllExpectedSwitchHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := &config.Config{}
	handler := NewGetAllExpectedSwitchHandler(dbSession, cfg)

	org := "test-org"
	infraProv, site := testExpectedSwitchSetupTestData(t, dbSession, org)

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

	// Create expected switches - one on managed site, one on unmanaged site
	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	managedES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "00:11:22:33:44:AA",
		SwitchSerialNumber: "MANAGED-SWITCH",
		Labels:             map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, managedES)

	unmanagedES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             unmanagedSite.ID,
		BmcMacAddress:      "00:11:22:33:44:BB",
		SwitchSerialNumber: "UNMANAGED-SWITCH",
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedES)

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
				var response []model.APIExpectedSwitch
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, es := range response {
					assert.NotEqual(t, unmanagedES.ID, es.ID, "Unmanaged switch should not be in response")
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
				var response []model.APIExpectedSwitch
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, es := range response {
					assert.NotEqual(t, unmanagedES.ID, es.ID, "Unmanaged switch should not be in response")
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
				var response []model.APIExpectedSwitch
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				for _, es := range response {
					assert.Equal(t, site.ID, es.SiteID, "All results should be from the specified site")
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
				var response []model.APIExpectedSwitch
				err := json.Unmarshal(body, &response)
				assert.Nil(t, err)
				assert.Greater(t, len(response), 0, "Should return at least one expected switch")
				for _, es := range response {
					assert.NotNil(t, es.Site, "Site relation should be loaded")
					assert.Equal(t, es.SiteID.String(), es.Site.ID, "Site ID should match")
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
			url := "/v2/org/" + org + "/nico/expected-switch"
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

func TestGetExpectedSwitchHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()

	cfg := &config.Config{}
	handler := NewGetExpectedSwitchHandler(dbSession, cfg)

	org := "test-org"
	infraProv, site := testExpectedSwitchSetupTestData(t, dbSession, org)

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

	// Create a test ExpectedSwitch on managed site
	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	testES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "TEST-SWITCH-123",
		Labels:             map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, testES)

	// Create a test ExpectedSwitch on unmanaged site
	unmanagedES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             unmanagedSite.ID,
		BmcMacAddress:      "00:11:22:33:44:CC",
		SwitchSerialNumber: "UNMANAGED-SWITCH-456",
		Labels:             map[string]string{"env": "unmanaged"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedES)

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
			id:   testES.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
		},
		{
			name: "switch not found",
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
			id:   unmanagedES.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedES.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/expected-switch/" + tt.id
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
				var response model.APIExpectedSwitch
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.NotNil(t, response.Labels, "Labels should not be nil in response")
				assert.Equal(t, "test", response.Labels["env"], "Labels in response should contain the 'env' label with value 'test'")
			}
		})
	}
}

func TestUpdateExpectedSwitchHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site := testExpectedSwitchSetupTestData(t, dbSession, org)

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

	// Create a test ExpectedSwitch on managed site
	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	testES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "00:11:22:33:44:DD",
		SwitchSerialNumber: "UPDATE-SWITCH-123",
		Labels:             map[string]string{"env": "test"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, testES)

	// Create a test ExpectedSwitch on unmanaged site
	unmanagedES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             unmanagedSite.ID,
		BmcMacAddress:      "00:11:22:33:44:EE",
		SwitchSerialNumber: "UNMANAGED-UPDATE-456",
		Labels:             map[string]string{"env": "unmanaged"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedES)

	// Create a second ExpectedSwitch on the managed site whose NVOS MAC the
	// update conflict test tries to claim
	conflictES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "00:11:22:33:44:CC",
		SwitchSerialNumber: "CONFLICT-SWITCH-789",
		NvosMacAddresses:   []string{"AA:BB:CC:DD:EE:33"},
	})
	assert.Nil(t, err)
	assert.NotNil(t, conflictES)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	workflowResult := mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	var capturedPatch *corev1.PatchExpectedSwitchRequest
	var capturedProxy grpcproxy.Request
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			capturedPatch = &corev1.PatchExpectedSwitchRequest{}
			testDecodeExpectedComponentPatch(t, args.Get(3), site.ID.String(), capturedPatch)
			capturedProxy = args.Get(3).(grpcproxy.Request)
			assert.Equal(t, corev1.Forge_PatchExpectedSwitch_FullMethodName, capturedProxy.FullMethod)
		}).
		Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewUpdateExpectedSwitchHandler(dbSession, scp, cfg)

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

	type testCase struct {
		name                     string
		id                       string
		requestBody              model.APIExpectedSwitchUpdateRequest
		rawRequestBody           string
		initialBmcIPAddress      *string
		expectedBmcIPAddress     *string
		setupContext             func(c echo.Context)
		expectedStatus           int
		expectedBmcMac           string
		expectedStoredBmcMac     string
		expectedErrorMsg         string
		expectNoWorkflow         bool
		expectedPaths            []string
		rejectsCredentials       bool
		assertPrimaryCleared     bool
		coreError                error
		checkNvosIPAddress       bool
		initialNvosInterfaces    model.APINvosInterfaces
		expectedNvosIPAddress    *string
		expectedNvosMacAddresses []string
	}
	tests := []testCase{
		{
			name: "metadata update excludes null BMC and NVOS credentials",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: cutil.GetPtr("UPDATED-SWITCH-123"),
				Labels:             map[string]string{"env": "updated"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusOK,
			expectedPaths:        []string{"metadata.labels", "switch_serial_number"},
			initialBmcIPAddress:  cutil.GetPtr("192.168.1.100"),
			expectedBmcIPAddress: cutil.GetPtr("192.168.1.100"),
		},
		{
			name: "255-character BMC password is encrypted without resubmitting its username",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("b", 255)),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"bmc_password"},
		},
		{
			name: "512-character BMC username is encrypted without resubmitting its password",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("v", 512)),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"bmc_username"},
		},
		{
			name: "NVOS password is encrypted without resubmitting its username",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvOsPassword: cutil.GetPtr("nvos-patch-secret"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"nvos_password"},
		},
		{
			name: "NVOS username is encrypted without resubmitting its password",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvOsUsername: cutil.GetPtr("nvos-patch-admin"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"nvos_username"},
		},
		{
			name: "Core credential rejection rolls back metadata",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvOsPassword: cutil.GetPtr("nvos-patch-secret"),
				Labels:       map[string]string{"env": "must-not-change"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:     http.StatusBadRequest,
			expectedErrorMsg:   "nvos_username and nvos_password must be set together",
			expectedPaths:      []string{"metadata.labels", "nvos_password"},
			rejectsCredentials: true,
			coreError:          swe.WrapErr(status.Error(codes.InvalidArgument, "nvos_username and nvos_password must be set together")),
		},
		{
			name: "empty NVOS username rejects accompanying metadata before dispatch",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvOsUsername: cutil.GetPtr(""),
				Labels:       map[string]string{"env": "must-not-change"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:     http.StatusBadRequest,
			expectedErrorMsg:   "nvOsUsername",
			expectNoWorkflow:   true,
			rejectsCredentials: true,
		},
		{
			name: "BMC MAC formatting difference preserves stored identity",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				BmcMacAddress: cutil.GetPtr("00-11-22-33-44-dd"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusOK,
			expectedBmcMac:       testES.BmcMacAddress,
			expectedStoredBmcMac: testES.BmcMacAddress,
		},
		{
			name: "BMC MAC address cannot be changed",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				BmcMacAddress: cutil.GetPtr("AA:BB:CC:DD:EE:FF"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusBadRequest,
			expectedStoredBmcMac: testES.BmcMacAddress,
			expectedErrorMsg:     "BMC MAC address cannot be changed after creation",
			expectNoWorkflow:     true,
		},
		{
			name: "successful update with BmcIpAddress",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				BmcIpAddress: cutil.GetPtr("192.168.1.42"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusOK,
			expectedPaths:        []string{"bmc_ip_address"},
			initialBmcIPAddress:  cutil.GetPtr("192.168.1.100"),
			expectedBmcIPAddress: cutil.GetPtr("192.168.1.42"),
		},
		{
			name:           "omitted BMC IP address preserves the stored address",
			id:             testES.ID.String(),
			rawRequestBody: `{"labels":{"env":"omitted-bmc-ip"}}`,
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusOK,
			expectedPaths:        []string{"metadata.labels"},
			initialBmcIPAddress:  cutil.GetPtr("192.168.1.100"),
			expectedBmcIPAddress: cutil.GetPtr("192.168.1.100"),
		},
		{
			name: "empty BMC IP address clears the stored address",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				BmcIpAddress: cutil.GetPtr(""),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:      http.StatusOK,
			expectedPaths:       []string{"bmc_ip_address"},
			initialBmcIPAddress: cutil.GetPtr("192.168.1.100"),
		},
		{
			name: "Core rejection rolls back clearing the BMC IP address",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				BmcIpAddress: cutil.GetPtr(""),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusBadRequest,
			expectedPaths:        []string{"bmc_ip_address"},
			expectedErrorMsg:     "BMC IP update rejected",
			initialBmcIPAddress:  cutil.GetPtr("192.168.1.100"),
			expectedBmcIPAddress: cutil.GetPtr("192.168.1.100"),
			coreError:            swe.WrapErr(status.Error(codes.InvalidArgument, "BMC IP update rejected")),
		},
		{
			name: "successful update with NvosInterfaces",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvosInterfaces:   model.APINvosInterfaces{{MacAddress: "00:11:22:33:44:66"}, {MacAddress: "00:11:22:33:44:67"}},
				NvosMacAddresses: json.RawMessage(`{"ignored":true}`),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:       http.StatusOK,
			expectedPaths:        []string{"nvos_interfaces", "nvos_ip_address"},
			assertPrimaryCleared: true,
		},
		{
			name: "update re-asserting its own NvosInterfaces is not a conflict",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvosInterfaces: model.APINvosInterfaces{{MacAddress: "00:11:22:33:44:66"}, {MacAddress: "00:11:22:33:44:67"}},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
			expectedPaths:  []string{"nvos_interfaces", "nvos_ip_address"},
		},
		{
			name: "update claiming another switch's NVOS MAC should return 409",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvosInterfaces: model.APINvosInterfaces{{MacAddress: "aa-bb-cc-dd-ee-33"}},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusConflict,
		},
		{
			name: "deprecated nonempty MAC list preserves ports and primary",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvosMacAddresses: json.RawMessage(`["AA:BB:CC:DD:EE:33", "invalid", "invalid"]`),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
		},
		{
			name: "deprecated empty MAC list preserves ports and primary",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvosMacAddresses: json.RawMessage(`[]`),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusOK,
		},
		{
			name: "update rejects clearing NvosInterfaces with an explicit empty list",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				NvosInterfaces: model.APINvosInterfaces{},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:   http.StatusBadRequest,
			expectNoWorkflow: true,
		},
		{
			name: "body ID mismatch with URL should return 400",
			id:   testES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				ID:                 cutil.GetPtr(uuid.New().String()),
				SwitchSerialNumber: cutil.GetPtr("SHOULD-NOT-UPDATE"),
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name: "cannot update on unmanaged site",
			id:   unmanagedES.ID.String(),
			requestBody: model.APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: cutil.GetPtr("SHOULD-NOT-UPDATE"),
				Labels:             map[string]string{"env": "fail"},
			},
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedES.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tc := range []struct {
		name            string
		body            string
		status          int
		wantIP          *string
		wantMACs        []string
		paths           []string
		errorText       string
		coreError       error
		initialMultiple bool
	}{
		{
			name:            "replace multiple ports with one port and an IPv6 address",
			body:            `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01","ipAddress":"2001:db8::20"}]}`,
			status:          http.StatusOK,
			wantIP:          cutil.GetPtr("2001:db8::20"),
			wantMACs:        []string{"02:00:00:00:21:01"},
			paths:           []string{"nvos_interfaces", "nvos_ip_address"},
			initialMultiple: true,
		},
		{
			name:      "reject NVOS IP on multiple replacement ports",
			body:      `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01"},{"macAddress":"02:00:00:00:21:02","ipAddress":"192.0.2.21"}]}`,
			status:    http.StatusBadRequest,
			wantIP:    cutil.GetPtr("192.0.2.20"),
			errorText: "ipAddress requires exactly one nvosInterfaces entry",
		},
		{
			name:      "reject multicast NVOS IP",
			body:      `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01","ipAddress":"ff02::1"}]}`,
			status:    http.StatusBadRequest,
			wantIP:    cutil.GetPtr("192.0.2.20"),
			errorText: "ipAddress must be a valid IPv4 or IPv6 address that is not unspecified, multicast, or limited broadcast",
		},
		{
			name:      "reject malformed NVOS IP",
			body:      `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01","ipAddress":"bad-address"}]}`,
			status:    http.StatusBadRequest,
			wantIP:    cutil.GetPtr("192.0.2.20"),
			errorText: "ipAddress must be a valid IPv4 or IPv6 address",
		},
		{
			name:      "reject empty NVOS IP",
			body:      `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01","ipAddress":""}]}`,
			status:    http.StatusBadRequest,
			wantIP:    cutil.GetPtr("192.0.2.20"),
			errorText: "ipAddress must be a valid IPv4 or IPv6 address",
		},
		{
			name:     "omitted interface list preserves NVOS IP",
			body:     `{"name":"retained-address"}`,
			status:   http.StatusOK,
			wantIP:   cutil.GetPtr("192.0.2.20"),
			wantMACs: []string{"02:00:00:00:21:01"},
			paths:    []string{"metadata.name"},
		},
		{
			name:     "null interface list preserves NVOS IP",
			body:     `{"nvosInterfaces":null}`,
			status:   http.StatusOK,
			wantIP:   cutil.GetPtr("192.0.2.20"),
			wantMACs: []string{"02:00:00:00:21:01"},
		},
		{
			name:     "replacement port without IP clears NVOS IP",
			body:     `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01"}]}`,
			status:   http.StatusOK,
			wantMACs: []string{"02:00:00:00:21:01"},
			paths:    []string{"nvos_interfaces", "nvos_ip_address"},
		},
		{
			name:     "replacement port with null IP clears NVOS IP",
			body:     `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01","ipAddress":null}]}`,
			status:   http.StatusOK,
			wantMACs: []string{"02:00:00:00:21:01"},
			paths:    []string{"nvos_interfaces", "nvos_ip_address"},
		},
		{
			name:     "multiple replacement ports clear NVOS IP",
			body:     `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01"},{"macAddress":"02:00:00:00:21:02"}]}`,
			status:   http.StatusOK,
			wantMACs: []string{"02:00:00:00:21:01", "02:00:00:00:21:02"},
			paths:    []string{"nvos_interfaces", "nvos_ip_address"},
		},
		{
			name:      "Core rejection rolls back changing NVOS IP",
			body:      `{"nvosInterfaces":[{"macAddress":"02:00:00:00:21:01","ipAddress":"192.0.2.21"}]}`,
			status:    http.StatusBadRequest,
			wantIP:    cutil.GetPtr("192.0.2.20"),
			paths:     []string{"nvos_interfaces", "nvos_ip_address"},
			errorText: "NVOS IP update rejected",
			coreError: swe.WrapErr(status.Error(codes.InvalidArgument, "NVOS IP update rejected")),
		},
	} {
		initial := model.APINvosInterfaces{{
			MacAddress: "02:00:00:00:21:01",
			IPAddress:  cutil.GetPtr("192.0.2.20"),
		}}
		if tc.initialMultiple {
			initial[0].IPAddress = nil
			initial = append(initial, model.APINvosInterface{MacAddress: "02:00:00:00:21:02"})
		}
		tests = append(tests, testCase{
			name:           tc.name,
			id:             testES.ID.String(),
			rawRequestBody: tc.body,
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus:           tc.status,
			expectedPaths:            tc.paths,
			expectedErrorMsg:         tc.errorText,
			expectNoWorkflow:         tc.status == http.StatusBadRequest && tc.coreError == nil,
			coreError:                tc.coreError,
			checkNvosIPAddress:       true,
			initialNvosInterfaces:    initial,
			expectedNvosIPAddress:    tc.wantIP,
			expectedNvosMacAddresses: tc.wantMACs,
		})
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.checkNvosIPAddress {
				_, seedErr := esDAO.Clear(ctx, nil, cdbm.ExpectedSwitchClearInput{
					ExpectedSwitchID:      testES.ID,
					NvosIpAddress:         true,
					NvosPrimaryMacAddress: true,
				})
				require.NoError(t, seedErr)
				_, seedErr = esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
					ExpectedSwitchID: testES.ID,
					NvosMacAddresses: tt.initialNvosInterfaces.MacAddresses(),
					NvosIpAddress:    tt.initialNvosInterfaces.IPAddress(),
				})
				require.NoError(t, seedErr)
			}
			ignoresLegacyPorts := tt.requestBody.NvosMacAddresses != nil && tt.requestBody.NvosInterfaces == nil
			if ignoresLegacyPorts {
				_, err := esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
					ExpectedSwitchID:      testES.ID,
					NvosMacAddresses:      []string{"00:11:22:33:44:66", "00:11:22:33:44:67"},
					NvosPrimaryMacAddress: cutil.GetPtr("00:11:22:33:44:67"),
				})
				require.NoError(t, err)
			}
			if tt.initialBmcIPAddress != nil {
				_, err := esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
					ExpectedSwitchID: testES.ID,
					BmcIpAddress:     tt.initialBmcIPAddress,
				})
				require.NoError(t, err)
			}
			reqBody, _ := json.Marshal(tt.requestBody)
			if tt.rawRequestBody != "" {
				reqBody = []byte(tt.rawRequestBody)
			}
			url := "/v2/org/" + org + "/nico/expected-switch/" + tt.id
			req := httptest.NewRequest(http.MethodPatch, url, bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req = req.WithContext(context.Background())

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			tt.setupContext(c)
			workflowResult.Return(tt.coreError)

			workflowCallsBefore := len(mockTemporalClient.Calls)
			var before *cdbm.ExpectedSwitch
			if tt.rejectsCredentials || tt.checkNvosIPAddress {
				before, err = esDAO.Get(ctx, nil, testES.ID, nil, false)
				require.NoError(t, err)
			}
			err := handler.Handle(c)

			assert.Nil(t, err)
			assert.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}
			if tt.expectedErrorMsg != "" {
				assert.Contains(t, rec.Body.String(), tt.expectedErrorMsg)
			}
			if tt.expectedBmcMac != "" {
				var response model.APIExpectedSwitch
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.Equal(t, tt.expectedBmcMac, response.BmcMacAddress)
			}
			if tt.expectedStoredBmcMac != "" {
				stored, err := esDAO.Get(ctx, nil, testES.ID, nil, false)
				assert.Nil(t, err)
				assert.Equal(t, tt.expectedStoredBmcMac, stored.BmcMacAddress)
			}
			if tt.expectNoWorkflow {
				assert.Len(t, mockTemporalClient.Calls, workflowCallsBefore)
			}
			if tt.rejectsCredentials {
				after, readErr := esDAO.Get(ctx, nil, testES.ID, nil, false)
				require.NoError(t, readErr)
				assert.Equal(t, before, after, "invalid credentials must not mutate the cloud row")
			}
			if rec.Code == http.StatusOK || tt.coreError != nil {
				require.Len(t, mockTemporalClient.Calls, workflowCallsBefore+1)
				require.NotNil(t, capturedPatch)
				require.NotNil(t, capturedPatch.UpdateMask)
				assert.Equal(t, tt.expectedPaths, capturedPatch.UpdateMask.Paths)
				if tt.requestBody.BmcIpAddress != nil {
					assert.Equal(t, *tt.requestBody.BmcIpAddress, capturedPatch.ExpectedSwitch.BmcIpAddress)
				}
				if tt.requestBody.DefaultBmcUsername != nil {
					assert.Equal(t, *tt.requestBody.DefaultBmcUsername, capturedPatch.ExpectedSwitch.BmcUsername)
					testExpectedComponentPatchSecrets(t, capturedProxy, *tt.requestBody.DefaultBmcUsername)
					assert.NotContains(t, rec.Body.String(), *tt.requestBody.DefaultBmcUsername)
				} else {
					assert.Empty(t, capturedPatch.ExpectedSwitch.BmcUsername)
				}
				if tt.requestBody.DefaultBmcPassword != nil {
					assert.Equal(t, *tt.requestBody.DefaultBmcPassword, capturedPatch.ExpectedSwitch.BmcPassword)
					testExpectedComponentPatchSecrets(t, capturedProxy, *tt.requestBody.DefaultBmcPassword)
					assert.NotContains(t, rec.Body.String(), *tt.requestBody.DefaultBmcPassword)
				} else {
					assert.Empty(t, capturedPatch.ExpectedSwitch.BmcPassword)
				}
				assert.Equal(t, tt.requestBody.NvOsUsername, capturedPatch.ExpectedSwitch.NvosUsername)
				assert.Equal(t, tt.requestBody.NvOsPassword, capturedPatch.ExpectedSwitch.NvosPassword)
				if tt.assertPrimaryCleared {
					stored, readErr := esDAO.Get(ctx, nil, testES.ID, nil, false)
					require.NoError(t, readErr)
					assert.Nil(t, stored.NvosPrimaryMacAddress)
					require.Len(t, capturedPatch.ExpectedSwitch.NvosInterfaces, 2)
					assert.True(t, capturedPatch.ExpectedSwitch.NvosInterfaces[0].IsPrimary)
					assert.False(t, capturedPatch.ExpectedSwitch.NvosInterfaces[1].IsPrimary)
				}
				if tt.requestBody.NvOsUsername != nil {
					testExpectedComponentPatchSecrets(t, capturedProxy, *tt.requestBody.NvOsUsername)
					assert.NotContains(t, rec.Body.String(), *tt.requestBody.NvOsUsername)
				}
				if tt.requestBody.NvOsPassword != nil {
					testExpectedComponentPatchSecrets(t, capturedProxy, *tt.requestBody.NvOsPassword)
					assert.NotContains(t, rec.Body.String(), *tt.requestBody.NvOsPassword)
				}
			}

			if tt.checkNvosIPAddress {
				stored, readErr := esDAO.Get(ctx, nil, testES.ID, nil, false)
				require.NoError(t, readErr)
				assert.Equal(t, tt.expectedNvosIPAddress, stored.NvosIpAddress)
				if rec.Code != http.StatusOK {
					assert.Equal(t, before, stored, "rejected update must preserve the complete row")
				} else {
					var response model.APIExpectedSwitch
					require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
					assert.Equal(t, tt.expectedNvosMacAddresses, response.NvosInterfaces.MacAddresses(), "NVOS MACs in response should match the expected port list")
					assert.Equal(t, tt.expectedNvosIPAddress, response.NvosInterfaces.IPAddress())
					assert.Equal(t, tt.expectedNvosIPAddress, capturedPatch.ExpectedSwitch.NvosIpAddress)
					if tt.expectedNvosIPAddress == nil {
						assert.Contains(t, rec.Body.String(), `"ipAddress":null`)
					}
				}
				if tt.coreError != nil {
					assert.Equal(t, cutil.GetPtr("192.0.2.21"), capturedPatch.ExpectedSwitch.NvosIpAddress)
				}
			}

			if tt.initialBmcIPAddress != nil {
				stored, err := esDAO.Get(ctx, nil, testES.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, tt.expectedBmcIPAddress, stored.BmcIpAddress)
				if rec.Code == http.StatusOK {
					var response map[string]json.RawMessage
					err = json.Unmarshal(rec.Body.Bytes(), &response)
					require.NoError(t, err)
					require.Contains(t, response, "bmcIpAddress")
					expectedJSON, err := json.Marshal(tt.expectedBmcIPAddress)
					require.NoError(t, err)
					assert.JSONEq(t, string(expectedJSON), string(response["bmcIpAddress"]))
				}
			}

			// Verify a supplied non-empty NvosInterfaces round-trips through the
			// update response. An explicit empty list is rejected (400), so it
			// never reaches this success path.
			if tt.expectedStatus == http.StatusOK && tt.requestBody.NvosInterfaces != nil {
				var response model.APIExpectedSwitch
				err := json.Unmarshal(rec.Body.Bytes(), &response)
				assert.Nil(t, err)
				assert.Equal(t, tt.requestBody.NvosInterfaces.MacAddresses(), response.NvosInterfaces.MacAddresses(), "NVOS MACs in response should match request")
			}
			if ignoresLegacyPorts {
				require.NotNil(t, capturedPatch)
				assert.Len(t, capturedPatch.ExpectedSwitch.NvosInterfaces, 2)
				stored, readErr := esDAO.Get(ctx, nil, testES.ID, nil, false)
				require.NoError(t, readErr)
				assert.Equal(t, []string{"00:11:22:33:44:66", "00:11:22:33:44:67"}, stored.NvosMacAddresses)
				assert.Equal(t, cutil.GetPtr("00:11:22:33:44:67"), stored.NvosPrimaryMacAddress)
				var response map[string]json.RawMessage
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.JSONEq(t, `[{"macAddress":"00:11:22:33:44:66","isPrimary":false,"ipAddress":null},{"macAddress":"00:11:22:33:44:67","isPrimary":true,"ipAddress":null}]`, string(response["nvosInterfaces"]))
				assert.JSONEq(t, `["00:11:22:33:44:66","00:11:22:33:44:67"]`, string(response["nvosMacAddresses"]))
			}
		})
	}
}

func TestReplaceAllExpectedSwitchesHandler_Handle(t *testing.T) {
	dbSession := testExpectedSwitchInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	const org = "test-org"
	_, site := testExpectedSwitchSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("switch-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{
			Name:  org,
			Roles: []string{authz.ProviderAdminRole},
		}},
	}
	dao := cdbm.NewExpectedSwitchDAO(dbSession)
	_, err := dao.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "02:00:00:00:30:01",
		SwitchSerialNumber: "original",
		NvosMacAddresses:   []string{"02:00:00:00:31:01"},
		NvosIpAddress:      cutil.GetPtr("192.0.2.10"),
	})
	require.NoError(t, err)
	tests := []struct {
		name       string
		interfaces model.APINvosInterfaces
		status     int
		errorText  string
	}{
		{
			name: "one addressed switch alongside a multi-port switch",
			interfaces: model.APINvosInterfaces{{
				MacAddress: "02:00:00:00:31:02",
				IPAddress:  cutil.GetPtr("2001:db8::20"),
			}},
			status: http.StatusOK,
		},
		{
			name: "limited broadcast address preserves the whole site inventory",
			interfaces: model.APINvosInterfaces{{
				MacAddress: "02:00:00:00:31:02",
				IPAddress:  cutil.GetPtr("255.255.255.255"),
			}},
			status:    http.StatusBadRequest,
			errorText: "ipAddress must be a valid IPv4 or IPv6 address that is not unspecified, multicast, or limited broadcast",
		},
		{
			name: "invalid address preserves the whole site inventory",
			interfaces: model.APINvosInterfaces{{
				MacAddress: "02:00:00:00:31:02",
				IPAddress:  cutil.GetPtr("192.0.2.20/24"),
			}},
			status:    http.StatusBadRequest,
			errorText: "ipAddress must be a valid IPv4 or IPv6 address",
		},
		{
			name: "multi-port address preserves the whole site inventory",
			interfaces: model.APINvosInterfaces{
				{MacAddress: "02:00:00:00:31:02"},
				{
					MacAddress: "02:00:00:00:31:05",
					IPAddress:  cutil.GetPtr("192.0.2.20"),
				},
			},
			status:    http.StatusBadRequest,
			errorText: "ipAddress requires exactly one nvosInterfaces entry",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			before, _, readErr := dao.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{}, paginator.PageInput{}, nil)
			require.NoError(t, readErr)
			request := model.APIReplaceAllExpectedSwitchesRequest{
				SiteID: site.ID.String(),
				ExpectedSwitches: []*model.APIExpectedSwitchCreateRequest{
					{
						SiteID:             site.ID.String(),
						BmcMacAddress:      "02:00:00:00:30:02",
						SwitchSerialNumber: "single-port",
						NvosInterfaces:     tt.interfaces,
					},
					{
						SiteID:             site.ID.String(),
						BmcMacAddress:      "02:00:00:00:30:03",
						SwitchSerialNumber: "multi-port",
						NvosInterfaces: model.APINvosInterfaces{
							{MacAddress: "02:00:00:00:31:03"},
							{MacAddress: "02:00:00:00:31:04"},
						},
					},
				},
			}
			body, marshalErr := json.Marshal(request)
			require.NoError(t, marshalErr)
			client := &tmocks.Client{}
			run := &tmocks.WorkflowRun{}
			client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).Return(run, nil)
			run.On("Get", mock.Anything, mock.Anything).Return(nil)
			pool := sc.NewClientPool(nil)
			pool.IDClientMap[site.ID.String()] = client
			handler := NewReplaceAllExpectedSwitchesHandler(dbSession, pool, common.GetTestConfig())
			rec := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodPut, "/v2/org/test-org/nico/expected-switch/all", string(body), "", handler.Handle)
			require.Equal(t, tt.status, rec.Code, rec.Body.String())
			after, _, readErr := dao.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{}, paginator.PageInput{}, nil)
			require.NoError(t, readErr)
			if tt.errorText != "" {
				assert.Contains(t, rec.Body.String(), tt.errorText)
				assert.Empty(t, client.Calls)
				assert.Equal(t, before, after)
				return
			}
			require.Len(t, client.Calls, 1)
			var coreRequest corev1.ExpectedSwitchList
			testDecodeExpectedComponentPatch(t, client.Calls[0].Arguments.Get(3), site.ID.String(), &coreRequest)
			require.Len(t, coreRequest.ExpectedSwitches, 2)
			assert.Equal(t, tt.interfaces.IPAddress(), coreRequest.ExpectedSwitches[0].NvosIpAddress)
			assert.Nil(t, coreRequest.ExpectedSwitches[1].NvosIpAddress)
			var response []model.APIExpectedSwitch
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
			require.Len(t, response, 2)
			assert.Equal(t, tt.interfaces.IPAddress(), response[0].NvosInterfaces.IPAddress())
			assert.Nil(t, response[1].NvosInterfaces[0].IPAddress)
			assert.Nil(t, response[1].NvosInterfaces[1].IPAddress)
			require.Len(t, after, 2)
			stored, readErr := dao.Get(ctx, nil, response[0].ID, nil, false)
			require.NoError(t, readErr)
			assert.Equal(t, tt.interfaces.IPAddress(), stored.NvosIpAddress)
		})
	}
}

func TestDeleteExpectedSwitchHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	ctx := context.Background()
	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	infraProv, site := testExpectedSwitchSetupTestData(t, dbSession, org)

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

	// Create a test ExpectedSwitch on managed site (to be deleted)
	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	testES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "00:11:22:33:44:FF",
		SwitchSerialNumber: "DELETE-SWITCH-123",
	})
	assert.Nil(t, err)
	assert.NotNil(t, testES)

	// Create a test ExpectedSwitch on unmanaged site (should not be deletable)
	unmanagedES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             unmanagedSite.ID,
		BmcMacAddress:      "00:11:22:33:55:00",
		SwitchSerialNumber: "UNMANAGED-DELETE-456",
	})
	assert.Nil(t, err)
	assert.NotNil(t, unmanagedES)

	// Add mock temporal client for the site
	mockTemporalClient := &tmocks.Client{}
	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return("test-workflow-id")
	mockWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteExpectedSwitch", mock.Anything).Return(mockWorkflowRun, nil)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewDeleteExpectedSwitchHandler(dbSession, scp, cfg)

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
			id:   testES.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, testES.ID.String())
			},
			expectedStatus: http.StatusNoContent,
		},
		{
			name: "cannot delete on unmanaged site",
			id:   unmanagedES.ID.String(),
			setupContext: func(c echo.Context) {
				c.Set("user", createMockUser(org))
				c.SetParamNames("orgName", "id")
				c.SetParamValues(org, unmanagedES.ID.String())
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	_ = infraProv

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			url := "/v2/org/" + org + "/nico/expected-switch/" + tt.id
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
