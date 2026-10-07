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
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
)

func testTenantAccountInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// reset the tables needed for tenant account tests
func testTenantAccountSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create TenantAccount table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
}

func testTenantAccountBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)
	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Infrastructure Provider"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)
	return ip
}

func testTenantAccountBuildTenant(t *testing.T, dbSession *cdb.Session, name string, displayName string, user *cdbm.User) *cdbm.Tenant {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, cdbm.TenantCreateInput{
		Name:           name,
		DisplayName:    &displayName,
		Org:            name,
		OrgDisplayName: &displayName,
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)

	return tn
}

func testTenantAccountBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string, firstName string, lastName string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	OrgData := cdbm.OrgData{}
	for _, org := range orgs {
		OrgData[org] = cdbm.Org{
			ID:          123,
			Name:        org,
			DisplayName: org,
			OrgType:     "ENTERPRISE",
			Roles:       roles,
		}
	}

	u, err := uDAO.Create(
		context.Background(),
		nil,
		cdbm.UserCreateInput{
			AuxiliaryID: nil,
			StarfleetID: &starfleetID,
			Email:       cutil.GetPtr(fmt.Sprintf("%s.%s@test.com", firstName, lastName)),
			FirstName:   cutil.GetPtr(firstName),
			LastName:    cutil.GetPtr(lastName),
			OrgData:     OrgData,
		},
	)
	assert.Nil(t, err)

	return u
}

func testTenantAccountBuildTenantAccount(t *testing.T, dbSession *cdb.Session, acctNum string, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, tenantOrg string, status string, createdBy uuid.UUID, contactUUID uuid.UUID) *cdbm.TenantAccount {
	taDAO := cdbm.NewTenantAccountDAO(dbSession)

	var tenantID *uuid.UUID
	if tenant != nil {
		tenantID = &tenant.ID
	}

	ta, err := taDAO.Create(context.Background(), nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             acctNum,
		TenantID:                  tenantID,
		TenantOrg:                 tenantOrg,
		InfrastructureProviderID:  ip.ID,
		InfrastructureProviderOrg: ip.Org,
		Status:                    status,
		CreatedBy:                 createdBy,
	})
	assert.Nil(t, err)

	if contactUUID != uuid.Nil {
		ta, err = taDAO.Update(context.Background(), nil, cdbm.TenantAccountUpdateInput{
			TenantAccountID: ta.ID,
			TenantContactID: &contactUUID,
		})
		assert.NoError(t, err)
	}
	return ta
}

func testTenantAccountBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID uuid.UUID, status string) {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID.String(), Status: status, Message: nil})
	assert.Nil(t, err)
	assert.NotNil(t, ssd)
	assert.Equal(t, entityID.String(), ssd.EntityID)
	assert.Equal(t, status, ssd.Status)
}

func testTenantAccountBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
	stDAO := cdbm.NewSiteDAO(dbSession)

	st, err := stDAO.Create(context.Background(), nil, cdbm.SiteCreateInput{
		Name:                          name,
		DisplayName:                   cutil.GetPtr("Test Site"),
		Description:                   cutil.GetPtr("Test Site Description"),
		Org:                           ip.Org,
		InfrastructureProviderID:      ip.ID,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:             false,
		SerialConsoleHostname:         cutil.GetPtr("TestSshHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Status:                        cdbm.SiteStatusPending,
		CreatedBy:                     user.ID,
	})
	assert.Nil(t, err)

	return st
}

func testTenantAccountBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
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

func TestTenantAccountHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()

	testTenantAccountSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)

	tn := testTenantAccountBuildTenant(t, dbSession, tnOrg1, "Tenant Org 1", tnu)
	assert.NotNil(t, tn)
	tn2 := testTenantAccountBuildTenant(t, dbSession, tnOrg2, "Tenant Org 2", tnu)
	assert.NotNil(t, tn2)

	// missing infrastructure provider id
	type ErrReq1 struct {
		// TenantID is the id of the tenant
		TenantID *string `json:"tenantId"`
		// TenantOrg is the org of the tenant
		TenantOrg *string `json:"tenantOrg"`
	}
	// missing tenant id and org
	type ErrReq2 struct {
		// InfrastructureProviderID is the id of the infrastructureProvider in the org
		InfrastructureProviderID string `json:"infrastructureProviderId"`
	}

	errBody1, err := json.Marshal(&ErrReq1{TenantID: cutil.GetPtr("some"), TenantOrg: cutil.GetPtr("some")})
	assert.Nil(t, err)
	errBody2, err := json.Marshal(&ErrReq2{InfrastructureProviderID: ip.ID.String()})
	assert.Nil(t, err)
	errBody3, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: "something"})
	assert.Nil(t, err)
	errBody4, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: ip2.ID.String(), TenantID: cutil.GetPtr(tn.ID.String())})
	assert.Nil(t, err)
	errBody5, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: ip.ID.String(), TenantID: cutil.GetPtr("somestr")})
	assert.Nil(t, err)
	errBody6, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: ip.ID.String(), TenantID: cutil.GetPtr(uuid.New().String())})
	assert.Nil(t, err)
	okBody1, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: ip.ID.String(), TenantID: cutil.GetPtr(tn.ID.String())})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: ip.ID.String(), TenantOrg: cutil.GetPtr(tnOrg2)})
	assert.Nil(t, err)
	okBody3, err := json.Marshal(model.APITenantAccountCreateRequest{InfrastructureProviderID: ip.ID.String(), TenantOrg: cutil.GetPtr(tnOrg3)})
	assert.Nil(t, err)
	okBodyInferIP, err := json.Marshal(model.APITenantAccountCreateRequest{TenantOrg: cutil.GetPtr("test-tn-org-infer")})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		reqBody            string
		user               *cdbm.User
		expectedErr        bool
		expectedStatus     int
		expectedTenantID   *uuid.UUID
		verifyChildSpanner bool
	}{
		{
			name:           "error when request body doesnt bind",
			reqOrgName:     ipOrg1,
			reqBody:        "SomeNonJsonBody",
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when infrastructureProvider not specified in req body",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody1),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when infrastructureProvider is invalid uuid",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody3),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when org has no infrastructure provider",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBody1),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when infrastructure in request does not match that in org",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody4),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when neither tenant id nor tenant org is specified in req body",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody2),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant id specified but not valid uuid",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody5),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant id specified but not found",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody6),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case when tenant id specified",
			reqOrgName:         ipOrg1,
			reqBody:            string(okBody1),
			user:               ipu,
			expectedErr:        false,
			expectedStatus:     http.StatusCreated,
			expectedTenantID:   &tn.ID,
			verifyChildSpanner: true,
		},
		{
			name:           "error when tenant account already exists for infrastructure provider, tenant",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:             "success case when tenant org specified and Tenant exists",
			reqOrgName:       ipOrg1,
			reqBody:          string(okBody2),
			user:             ipu,
			expectedErr:      false,
			expectedStatus:   http.StatusCreated,
			expectedTenantID: &tn2.ID,
		},
		{
			name:             "success case when tenant org specified and Tenant does not exist",
			reqOrgName:       ipOrg1,
			reqBody:          string(okBody3),
			user:             ipu,
			expectedErr:      false,
			expectedStatus:   http.StatusCreated,
			expectedTenantID: nil,
		},
		{
			name:             "success when infrastructureProviderId is omitted (inferred from org)",
			reqOrgName:       ipOrg1,
			reqBody:          string(okBodyInferIP),
			user:             ipu,
			expectedErr:      false,
			expectedStatus:   http.StatusCreated,
			expectedTenantID: nil,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := CreateTenantAccountHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			require.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)

			if !tc.expectedErr {
				rsp := &model.APITenantAccount{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 1)
				assert.Equal(t, ip.ID.String(), rsp.InfrastructureProviderID)

				if tc.expectedTenantID != nil {
					assert.Equal(t, tc.expectedTenantID.String(), *rsp.TenantID)
				}
				assert.Nil(t, rsp.TenantContact)
				assert.Equal(t, cdbm.TenantAccountStatusInvited, rsp.Status)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()

	testTenantAccountSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"

	ipOrgRoles := []string{authz.ProviderAdminRole}
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipUser := testTenantAccountBuildUser(t, dbSession, "test123", []string{ipOrg}, ipOrgRoles, "John", "Doe")
	tnUser := testTenantAccountBuildUser(t, dbSession, "test456", []string{tnOrg1, tnOrg2, tnOrg3, tnOrg4}, tnOrgRoles, "Jimmy", "Doe")

	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipUser)
	assert.NotNil(t, ip)

	tn1 := testTenantAccountBuildTenant(t, dbSession, tnOrg1, "Test Tenant Account 1", ipUser)
	assert.NotNil(t, tn1)

	tn2 := testTenantAccountBuildTenant(t, dbSession, tnOrg2, "Test Tenant Account 2", ipUser)
	assert.NotNil(t, tn2)

	tn3 := testTenantAccountBuildTenant(t, dbSession, tnOrg3, "Test Tenant Account 3", ipUser)
	assert.NotNil(t, tn2)

	ta1 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip, tn1, tnOrg1, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta1)
	testTenantAccountBuildStatusDetail(t, dbSession, ta1.ID, ta1.Status)

	ta2 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip, tn2, tnOrg2, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta2)
	testTenantAccountBuildStatusDetail(t, dbSession, ta2.ID, ta2.Status)

	ta3 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip, tn3, tnOrg3, cdbm.TenantAccountStatusReady, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta3)

	errBody1, err := json.Marshal(model.APITenantAccountUpdateRequest{TenantContactID: cutil.GetPtr("non-uuid$!")})
	assert.Nil(t, err)
	errBody2, err := json.Marshal(model.APITenantAccountUpdateRequest{TenantContactID: cutil.GetPtr(uuid.New().String())})
	assert.Nil(t, err)

	okBody1, err := json.Marshal(model.APITenantAccountUpdateRequest{})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(model.APITenantAccountUpdateRequest{TenantContactID: cutil.GetPtr(tnUser.ID.String())})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		reqBody            string
		user               *cdbm.User
		tnID               string
		taID               string
		ta                 *cdbm.TenantAccount
		expectedErr        bool
		expectedStatus     int
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           nil,
			tnID:           tn1.ID.String(),
			taID:           ta1.ID.String(),
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when tenantContactId is invalid uuid",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBody1),
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           "someB*d(uuid",
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when reqBody doesnt bind",
			reqOrgName:     tnOrg1,
			reqBody:        "BadBody",
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           ta1.ID.String(),
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when reqBody json doesnt validate",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBody1),
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           ta1.ID.String(),
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenantContactId in request does not match requesting user",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBody2),
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           ta1.ID.String(),
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have tenant",
			reqOrgName:     tnOrg4,
			reqBody:        string(okBody1),
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           ta1.ID.String(),
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when specified org does not have matching tenant in tenant account",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           ta2.ID.String(),
			ta:             ta2,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified tenant account doesnt exist",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           tnUser,
			tnID:           tn1.ID.String(),
			taID:           uuid.New().String(),
			ta:             ta1,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when specified tenant account is not in Invited status",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           tnUser,
			tnID:           tn3.ID.String(),
			taID:           ta3.ID.String(),
			ta:             ta3,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case when tenantContactId specified in body",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody2),
			user:               tnUser,
			tnID:               tn1.ID.String(),
			taID:               ta1.ID.String(),
			ta:                 ta1,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name:           "success case when tenantContactId id not specified in body",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody1),
			user:           tnUser,
			tnID:           tn2.ID.String(),
			taID:           ta2.ID.String(),
			ta:             ta2,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrgName, tc.taID)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := UpdateTenantAccountHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			require.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if !tc.expectedErr {
				rsp := &model.APITenantAccount{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, 2, len(rsp.StatusHistory))
				assert.Equal(t, ip.ID.String(), rsp.InfrastructureProviderID)
				assert.Equal(t, tc.tnID, *rsp.TenantID)
				assert.Equal(t, cdbm.TenantAccountStatusReady, rsp.Status)
				assert.NotEqual(t, rsp.Updated.String(), tc.ta.Updated.String())
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountHandler_UpdateSiteCapabilities(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()

	testTenantAccountSetupSchema(t, dbSession)

	ipOrg := "test-capabilities-ip-org"
	tenantOrg := "test-capabilities-tenant-org"
	ipUser := testTenantAccountBuildUser(t, dbSession, "test-capabilities-user", []string{ipOrg}, []string{authz.ProviderAdminRole}, "Site", "Admin")
	ip := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Capabilities Infrastructure Provider", ipOrg, ipUser)
	tenant := testTenantAccountBuildTenant(t, dbSession, tenantOrg, "Test Capabilities Tenant", ipUser)
	tenantAccount := testTenantAccountBuildTenantAccount(
		t,
		dbSession,
		uuid.New().String(),
		ip,
		tenant,
		tenantOrg,
		cdbm.TenantAccountStatusReady,
		ipUser.ID,
		uuid.Nil,
	)
	site := testTenantAccountBuildSite(t, dbSession, ip, "test-capabilities-site", ipUser)
	tenantSite := cdbm.TestBuildTenantSite(t, dbSession, tenant, site, nil, ipUser)

	handler := NewUpdateTenantAccountHandler(dbSession, &tmocks.Client{}, common.GetTestConfig())
	sendUpdate := func(caps model.APITenantAccountSiteCapabilitiesUpdateRequest) (*httptest.ResponseRecorder, *model.APITenantAccount) {
		t.Helper()

		body, err := json.Marshal(model.APITenantAccountUpdateRequest{SiteCapabilities: &caps})
		require.NoError(t, err)

		e := echo.New()
		req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(body)))
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		rec := httptest.NewRecorder()
		ec := e.NewContext(req, rec)
		ec.SetParamNames("orgName", "id")
		ec.SetParamValues(ipOrg, tenantAccount.ID.String())
		ec.Set("user", ipUser)
		ec.SetRequest(ec.Request().WithContext(ctx))

		require.NoError(t, handler.Handle(ec))
		if rec.Code != http.StatusOK {
			t.Logf("response: %s", rec.Body.String())
		}
		require.Equal(t, http.StatusOK, rec.Code)

		response := &model.APITenantAccount{}
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), response))
		return rec, response
	}

	siteOverrideCaps := model.APITenantAccountSiteCapabilitiesUpdateRequest{
		{
			SiteIDs:                  []string{},
			TargetedInstanceCreation: cutil.GetPtr(true),
		},
		{
			SiteIDs:                  []string{site.ID.String()},
			TargetedInstanceCreation: cutil.GetPtr(false),
		},
	}
	_, response := sendUpdate(siteOverrideCaps)
	assert.Equal(t, []model.APITenantAccountSiteCapability{
		{SiteIDs: []string{}, TargetedInstanceCreation: true},
		{
			SiteIDs:                  []string{site.ID.String()},
			TargetedInstanceCreation: false,
		},
	}, response.SiteCapabilities)

	tenantAccountDAO := cdbm.NewTenantAccountDAO(dbSession)
	updatedTenantAccount, err := tenantAccountDAO.GetByID(ctx, nil, tenantAccount.ID, nil)
	require.NoError(t, err)
	assert.True(t, updatedTenantAccount.Config.TargetedInstanceCreation)

	tenantSiteDAO := cdbm.NewTenantSiteDAO(dbSession)
	updatedTenantSite, err := tenantSiteDAO.GetByID(ctx, nil, tenantSite.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, updatedTenantSite.Config.TargetedInstanceCreation)
	assert.False(t, *updatedTenantSite.Config.TargetedInstanceCreation)

	accountOnlyCaps := model.APITenantAccountSiteCapabilitiesUpdateRequest{
		{
			TargetedInstanceCreation: cutil.GetPtr(false),
		},
	}
	_, response = sendUpdate(accountOnlyCaps)
	assert.Equal(t, []model.APITenantAccountSiteCapability{
		{SiteIDs: []string{}, TargetedInstanceCreation: false},
	}, response.SiteCapabilities)

	updatedTenantAccount, err = tenantAccountDAO.GetByID(ctx, nil, tenantAccount.ID, nil)
	require.NoError(t, err)
	assert.False(t, updatedTenantAccount.Config.TargetedInstanceCreation)

	updatedTenantSite, err = tenantSiteDAO.GetByID(ctx, nil, tenantSite.ID, nil)
	require.NoError(t, err)
	assert.Nil(t, updatedTenantSite.Config.TargetedInstanceCreation)
}

func TestTenantAccountHandler_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()

	testTenantAccountSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"

	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}
	tnRoles := []string{authz.TenantAdminRole}

	ipUser := testTenantAccountBuildUser(t, dbSession, "test123", []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles, "John", "Doe")
	ipvUser := testTenantAccountBuildUser(t, dbSession, "test1234", []string{ipOrg1, ipOrg2, ipOrg3}, ipvRoles, "Jimmy", "Doe")
	tnUser := testTenantAccountBuildUser(t, dbSession, "test456", []string{tnOrg1, tnOrg2, tnOrg3, tnOrg4}, tnRoles, "Tommy", "Doe")

	ip1 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 1", ipOrg1, ipUser)
	assert.NotNil(t, ip1)
	ip2 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 2", ipOrg2, ipUser)
	assert.NotNil(t, ip2)

	st1 := testTenantAccountBuildSite(t, dbSession, ip1, "Test Site 1", ipUser)
	assert.NotNil(t, st1)
	tn1 := testTenantAccountBuildTenant(t, dbSession, tnOrg1, "Test Tenant 1", tnUser)
	assert.NotNil(t, tn1)
	tn2 := testTenantAccountBuildTenant(t, dbSession, tnOrg2, "test Tenant 2", tnUser)
	assert.NotNil(t, tn2)
	tn3 := testTenantAccountBuildTenant(t, dbSession, tnOrg3, "Test Tenant 3", tnUser)
	assert.NotNil(t, tn3)

	ta11 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip1, tn1, tnOrg1, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta11)
	testTenantAccountBuildStatusDetail(t, dbSession, ta11.ID, ta11.Status)
	testTenantAccountBuildStatusDetail(t, dbSession, ta11.ID, cdbm.TenantAccountStatusReady)

	taDAO := cdbm.NewTenantAccountDAO(dbSession)
	_, err := taDAO.Update(ctx, nil, cdbm.TenantAccountUpdateInput{
		TenantAccountID: ta11.ID,
		TenantContactID: &ipUser.ID,
		Status:          cutil.GetPtr(cdbm.TenantAccountStatusReady),
	})
	assert.Nil(t, err)

	ta12 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip1, tn2, tnOrg2, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta12)
	testTenantAccountBuildStatusDetail(t, dbSession, ta12.ID, ta12.Status)

	ta13 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip1, tn3, tnOrg3, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta13)
	testTenantAccountBuildStatusDetail(t, dbSession, ta13.ID, ta13.Status)

	ta21 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip2, tn1, tnOrg1, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta21)
	testTenantAccountBuildStatusDetail(t, dbSession, ta21.ID, ta21.Status)

	ta22 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip2, tn2, tnOrg2, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta22)
	testTenantAccountBuildStatusDetail(t, dbSession, ta22.ID, ta22.Status)

	// Build Allocation
	tal1 := testTenantAccountBuildAllocation(t, dbSession, st1, tn1, "test Allocation 1", tnUser)
	assert.NotNil(t, tal1)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		taID                              string
		queryInfrastructureProviderID     *string
		queryTenantID                     *string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		expectedErr                       bool
		expectedStatus                    int
		expectedID                        string
		expectedStatusDetailsCnt          int
		expectedTenantContact             *cdbm.User
		expectedTenantOrg                 *string
		expectedInfrastructureProviderOrg *string
		expectedAllocationCount           int
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			taID:           ta11.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when tenant account id is invalid uuid",
			reqOrgName:     tnOrg1,
			user:           tnUser,
			taID:           "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant account id not found",
			reqOrgName:     tnOrg1,
			user:           tnUser,
			taID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:                          "error when infrastructure provider not found for org",
			reqOrgName:                    ipOrg3,
			user:                          ipUser,
			taID:                          ta11.ID.String(),
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
		},
		{
			name:                          "error when infrastructure provider in org doesnt match infrastructure provider in tenant account",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			taID:                          ta21.ID.String(),
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			expectedErr:                   true,
			expectedStatus:                http.StatusForbidden,
		},
		{
			name:           "error when tenant not found for org",
			reqOrgName:     tnOrg4,
			user:           tnUser,
			taID:           ta11.ID.String(),
			queryTenantID:  cutil.GetPtr(tn1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant id in url doesnt match org",
			reqOrgName:     tnOrg2,
			user:           tnUser,
			taID:           ta11.ID.String(),
			queryTenantID:  cutil.GetPtr(tn1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant in org doesnt match tenant in tenant account",
			reqOrgName:     tnOrg1,
			user:           tnUser,
			taID:           ta12.ID.String(),
			queryTenantID:  cutil.GetPtr(tn1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                     "error when tenant id is not found",
			reqOrgName:               tnOrg1,
			user:                     tnUser,
			taID:                     uuid.New().String(),
			queryTenantID:            cutil.GetPtr(tn1.ID.String()),
			expectedErr:              true,
			expectedStatus:           http.StatusNotFound,
			expectedID:               ta11.ID.String(),
			expectedStatusDetailsCnt: 2,
		},
		{
			name:                              "success when infrastructure provider id is specified",
			reqOrgName:                        ipOrg1,
			user:                              ipUser,
			taID:                              ta11.ID.String(),
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        ta11.ID.String(),
			expectedStatusDetailsCnt:          2,
			expectedTenantOrg:                 &tn1.Org,
			expectedInfrastructureProviderOrg: &ip1.Org,
			expectedTenantContact:             ipUser,
			expectedAllocationCount:           1,
			verifyChildSpanner:                true,
		},
		{
			name:                              "success when user has Provider viewer role",
			reqOrgName:                        ipOrg1,
			user:                              ipvUser,
			taID:                              ta11.ID.String(),
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        ta11.ID.String(),
			expectedStatusDetailsCnt:          2,
			expectedTenantOrg:                 &tn1.Org,
			expectedInfrastructureProviderOrg: &ip1.Org,
			expectedTenantContact:             ipUser,
			expectedAllocationCount:           1,
		},
		{
			name:                              "success when tenant id is specified",
			reqOrgName:                        tnOrg1,
			user:                              tnUser,
			taID:                              ta11.ID.String(),
			queryTenantID:                     cutil.GetPtr(tn1.ID.String()),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        ta11.ID.String(),
			expectedStatusDetailsCnt:          2,
			expectedTenantOrg:                 &tn1.Org,
			expectedInfrastructureProviderOrg: &ip1.Org,
			expectedTenantContact:             ipUser,
			expectedAllocationCount:           1,
		},
		{
			name:                              "success when both infrastructure id and tenant id are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipUser,
			taID:                              ta11.ID.String(),
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                     cutil.GetPtr(tn1.ID.String()),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        ta11.ID.String(),
			expectedStatusDetailsCnt:          2,
			expectedTenantOrg:                 &tn1.Org,
			expectedInfrastructureProviderOrg: &ip1.Org,
			expectedTenantContact:             ipUser,
			expectedAllocationCount:           1,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)
			q := req.URL.Query()
			if tc.queryInfrastructureProviderID != nil {
				q.Add("infrastructureProviderId", *tc.queryInfrastructureProviderID)
			}
			if tc.queryTenantID != nil {
				q.Add("tenantId", *tc.queryTenantID)
			}

			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}

			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.URL.RawQuery = q.Encode()
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.taID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := GetTenantAccountHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			require.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if !tc.expectedErr {
				rsp := &model.APITenantAccount{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedID, rsp.ID)
				assert.Equal(t, tc.expectedStatusDetailsCnt, len(rsp.StatusHistory))
				assert.Equal(t, *tc.expectedTenantOrg, rsp.TenantOrg)
				assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProviderOrg)
				assert.NotNil(t, tc.expectedTenantContact)
				assert.Equal(t, tc.expectedTenantContact.ID.String(), rsp.TenantContact.ID)
				assert.Equal(t, tc.expectedAllocationCount, rsp.AllocationCount)

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProvider.Org)
					}

					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
				} else {
					assert.Nil(t, rsp.Tenant)
					assert.Nil(t, rsp.InfrastructureProvider)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()

	testTenantAccountSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"

	totalCount := 30

	tnOrgs := []string{}
	for i := 0; i < totalCount/2+1; i++ {
		tnOrgs = append(tnOrgs, fmt.Sprintf("test-tn-org-%02d", i))
	}

	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}
	tnRoles := []string{authz.TenantAdminRole}

	ipUser := testTenantAccountBuildUser(t, dbSession, "test123", []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles, "John", "Doe")
	ipvUser := testTenantAccountBuildUser(t, dbSession, "test1234", []string{ipOrg1, ipOrg2, ipOrg3}, ipvRoles, "Jimmy", "Doe")
	tnUser := testTenantAccountBuildUser(t, dbSession, "test456", tnOrgs, tnRoles, "Tommy", "Doe")

	contactUser1 := testTenantAccountBuildUser(t, dbSession, "testUser2", []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles, "John", "Smith")
	contactUser2 := testTenantAccountBuildUser(t, dbSession, "testUser3", []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles, "John", "Brewer")

	ip1 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 1", ipOrg1, ipUser)
	assert.NotNil(t, ip1)
	ip2 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 2", ipOrg2, ipUser)
	assert.NotNil(t, ip2)

	st1 := testTenantAccountBuildSite(t, dbSession, ip1, "site1", ipUser)
	assert.NotNil(t, st1)
	st2 := testTenantAccountBuildSite(t, dbSession, ip2, "site2", ipUser)
	assert.NotNil(t, st2)

	tns := []cdbm.Tenant{}
	tas := []cdbm.TenantAccount{}

	for i := 0; i < totalCount/2; i++ {
		tn := testTenantAccountBuildTenant(t, dbSession, tnOrgs[i], fmt.Sprintf("Test Tenant Org %02d", i), tnUser)
		assert.NotNil(t, tn)
		tns = append(tns, *tn)

		allocation := testTenantAccountBuildAllocation(t, dbSession, st1, tn, "Test Allocation", ipUser)
		assert.NotNil(t, allocation)
		allocation2 := testTenantAccountBuildAllocation(t, dbSession, st2, tn, "Test Allocation 2", ipUser)
		assert.NotNil(t, allocation2)

		ta1 := testTenantAccountBuildTenantAccount(t, dbSession, fmt.Sprintf("test-tenant-account-%02d", i), ip1, tn, tn.Org, cdbm.TenantAccountStatusInvited, ipUser.ID, contactUser1.ID)
		assert.NotNil(t, ta1)

		common.TestBuildStatusDetail(t, dbSession, ta1.ID.String(), cdbm.TenantAccountStatusPending, cutil.GetPtr("request received, pending processing"))
		common.TestBuildStatusDetail(t, dbSession, ta1.ID.String(), cdbm.TenantAccountStatusReady, cutil.GetPtr("Tenant Account is now ready for use"))

		ta2 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip2, tn, tn.Org, cdbm.TenantAccountStatusInvited, ipUser.ID, contactUser2.ID)
		assert.NotNil(t, ta2)

		common.TestBuildStatusDetail(t, dbSession, ta2.ID.String(), cdbm.TenantAccountStatusPending, cutil.GetPtr("request received, pending processing"))
		common.TestBuildStatusDetail(t, dbSession, ta2.ID.String(), cdbm.TenantAccountStatusReady, cutil.GetPtr("Tenant Account is now ready for use"))

		tas = append(tas, *ta1, *ta2)
	}

	tn15 := testTenantAccountBuildTenant(t, dbSession, tnOrgs[15], fmt.Sprintf("Test Tenant Org %02d", 15), tnUser)
	assert.NotNil(t, tn15)

	taDAO := cdbm.NewTenantAccountDAO(dbSession)
	_, err := taDAO.Update(ctx, nil, cdbm.TenantAccountUpdateInput{
		TenantAccountID: tas[28].ID,
		TenantContactID: &ipUser.ID,
		Status:          cutil.GetPtr(cdbm.TenantAccountStatusReady),
	})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		queryInfrastructureProviderID     *string
		queryTenantID                     *string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		queryStatus                       *string
		querySearchQuery                  *string
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedAllocationCount           int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.TenantAccount
		expectedTenantOrg                 *string
		expectedInfrastructureProviderOrg *string
		expectedTenantContact             *cdbm.User
		verifyChildSpanner                bool
	}{
		{
			name:                          "error when user not found in request context",
			reqOrgName:                    ipOrg1,
			user:                          nil,
			queryInfrastructureProviderID: nil,
			queryTenantID:                 nil,
			queryIncludeRelations1:        nil,
			expectedErr:                   true,
			expectedStatus:                http.StatusInternalServerError,
		},
		{
			name:                          "error when infrastructure provider not found for org",
			reqOrgName:                    ipOrg3,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                 nil,
			queryIncludeRelations1:        nil,
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
		},
		{
			name:                          "success when infrastructure provider id is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                 nil,
			expectedErr:                   false,
			queryIncludeRelations1:        nil,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   totalCount / 2,
			verifyChildSpanner:            true,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success with whitespace-only search query",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			querySearchQuery:              cutil.GetPtr("   "),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   totalCount / 2,
			expectedTotal:                 cutil.GetPtr(totalCount / 2),
			expectedAllocationCount:       1,
		},
		{
			name:                          "success when tenant id is specified",
			reqOrgName:                    tnOrgs[0],
			user:                          tnUser,
			queryInfrastructureProviderID: nil,
			queryTenantID:                 cutil.GetPtr(tns[0].ID.String()),
			queryIncludeRelations1:        nil,
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   2,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success when both infrastructure id and tenant id are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                 cutil.GetPtr(tns[2].ID.String()),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   1,
			expectedAllocationCount:       1,
		},
		{
			name:                              "success when user has Provider viewer role",
			reqOrgName:                        ipOrg1,
			user:                              ipvUser,
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       totalCount / 2,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip1.Org),
			expectedAllocationCount:           1,
		},
		{
			name:                              "success when infrastructure id and relation are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipUser,
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       totalCount / 2,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip1.Org),
			expectedAllocationCount:           1,
		},
		{
			name:                          "success when tenant id and relation are specified",
			reqOrgName:                    tnOrgs[0],
			user:                          tnUser,
			queryInfrastructureProviderID: nil,
			queryTenantID:                 cutil.GetPtr(tns[0].ID.String()),
			queryIncludeRelations1:        cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   2,
			expectedTenantOrg:             cutil.GetPtr(tns[0].Org),
			expectedAllocationCount:       1,
		},
		{
			name:                              "success when both infrastructure and tenant id/relation are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipUser,
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                     cutil.GetPtr(tns[2].ID.String()),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       1,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip1.Org),
			expectedTenantOrg:                 cutil.GetPtr(tns[2].Org),
			expectedAllocationCount:           1,
		},
		{
			name:                          "failure when invalid relation params are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                 nil,
			queryIncludeRelations1:        cutil.GetPtr(cdbm.AllocationRelationName),
			queryIncludeRelations2:        cutil.GetPtr(cdbm.IPBlockRelationName),
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("TEST_ASC"),
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
			expectedCnt:                   0,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success when no results are returned",
			reqOrgName:                    tnOrgs[15],
			user:                          tnUser,
			queryInfrastructureProviderID: nil,
			queryTenantID:                 cutil.GetPtr(tn15.ID.String()),
			queryIncludeRelations1:        nil,
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   0,
			expectedAllocationCount:       1,
		},
		{
			name:                              "success when pagination params are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipUser,
			queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                     nil,
			queryIncludeRelations1:            nil,
			pageNumber:                        cutil.GetPtr(1),
			pageSize:                          cutil.GetPtr(10),
			orderBy:                           cutil.GetPtr("CREATED_DESC"),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       10,
			expectedTotal:                     cutil.GetPtr(totalCount / 2),
			expectedFirstEntry:                &tas[28],
			expectedTenantOrg:                 cutil.GetPtr(tas[28].TenantOrg),
			expectedInfrastructureProviderOrg: cutil.GetPtr(tas[28].InfrastructureProviderOrg),
			expectedTenantContact:             ipUser,
			expectedAllocationCount:           1,
		},
		{
			name:                          "failure when invalid pagination params are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryTenantID:                 nil,
			queryIncludeRelations1:        nil,
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("TEST_ASC"),
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
			expectedCnt:                   0,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success when TenantAccountStatusPending status are specified",
			reqOrgName:                    tnOrgs[0],
			user:                          tnUser,
			queryInfrastructureProviderID: nil,
			queryTenantID:                 cutil.GetPtr(tns[0].ID.String()),
			queryStatus:                   cutil.GetPtr(cdbm.TenantAccountStatusPending),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   0,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success when TenantAccountStatusReady status are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryStatus:                   cutil.GetPtr(cdbm.TenantAccountStatusReady),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   1,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success when BadStatus status are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			queryStatus:                   cutil.GetPtr("BadStatus"),
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
			expectedCnt:                   0,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success with provider-scoped search query matching account number",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			querySearchQuery:              cutil.GetPtr("test-tenant-account-00"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   1,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success with search query combined with status filter",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			querySearchQuery:              cutil.GetPtr("test-tenant-account-00"),
			queryStatus:                   cutil.GetPtr(cdbm.TenantAccountStatusInvited),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   1,
			expectedAllocationCount:       1,
		},
		{
			name:                          "success with search query no matches",
			reqOrgName:                    ipOrg1,
			user:                          ipUser,
			queryInfrastructureProviderID: cutil.GetPtr(ip1.ID.String()),
			querySearchQuery:              cutil.GetPtr("nonexistent-query-xyz"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   0,
			expectedTotal:                 cutil.GetPtr(0),
			expectedAllocationCount:       1,
		},
		{
			name:                    "success with tenant-scoped search query matching account number",
			reqOrgName:              tnOrgs[0],
			user:                    tnUser,
			queryTenantID:           cutil.GetPtr(tns[0].ID.String()),
			querySearchQuery:        cutil.GetPtr("test-tenant-account-00"),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedCnt:             1,
			expectedAllocationCount: 1,
		},
		{
			name:                    "success with tenant-scoped search query and status filter",
			reqOrgName:              tnOrgs[0],
			user:                    tnUser,
			queryTenantID:           cutil.GetPtr(tns[0].ID.String()),
			querySearchQuery:        cutil.GetPtr("test-tenant-account-00"),
			queryStatus:             cutil.GetPtr(cdbm.TenantAccountStatusInvited),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedCnt:             1,
			expectedAllocationCount: 1,
		},
		//{
		//	name:                              "success when sort by account number",
		//	reqOrgName:                        ipOrg1,
		//	user:                              ipUser,
		//	queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
		//	pageNumber:                        cutil.GetPtr(1),
		//	pageSize:                          cutil.GetPtr(10),
		//	orderBy:                           cutil.GetPtr("ACCOUNT_NUMBER_DESC"),
		//	expectedStatus:                    http.StatusOK,
		//	expectedCnt:                       10,
		//	expectedTotal:                     cutil.GetPtr(totalCount / 2),
		//	expectedFirstEntry:                &tas[28],
		//	expectedTenantOrg:                 cutil.GetPtr(tas[28].TenantOrg),
		//	expectedInfrastructureProviderOrg: cutil.GetPtr(tas[28].InfrastructureProviderOrg),
		//	expectedTenantContact:             ipUser,
		//	expectedAllocationCount:           1,
		//},
		//{
		//	name:                              "success when sort by tenant org name",
		//	reqOrgName:                        ipOrg1,
		//	user:                              ipUser,
		//	queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
		//	pageNumber:                        cutil.GetPtr(1),
		//	pageSize:                          cutil.GetPtr(10),
		//	orderBy:                           cutil.GetPtr("TENANT_ORG_NAME_DESC"),
		//	expectedStatus:                    http.StatusOK,
		//	expectedCnt:                       10,
		//	expectedTotal:                     cutil.GetPtr(totalCount / 2),
		//	expectedFirstEntry:                &tas[28],
		//	expectedTenantOrg:                 cutil.GetPtr(tas[28].TenantOrg),
		//	expectedInfrastructureProviderOrg: cutil.GetPtr(tas[28].InfrastructureProviderOrg),
		//	expectedTenantContact:             ipUser,
		//	expectedAllocationCount:           1,
		//},
		//{
		//	name:                              "success when sort by tenant org display name",
		//	reqOrgName:                        ipOrg1,
		//	user:                              ipUser,
		//	queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
		//	pageNumber:                        cutil.GetPtr(1),
		//	pageSize:                          cutil.GetPtr(10),
		//	orderBy:                           cutil.GetPtr("TENANT_ORG_DISPLAY_NAME_DESC"),
		//	expectedStatus:                    http.StatusOK,
		//	expectedCnt:                       10,
		//	expectedTotal:                     cutil.GetPtr(totalCount / 2),
		//	expectedFirstEntry:                &tas[28],
		//	expectedTenantOrg:                 cutil.GetPtr(tas[28].TenantOrg),
		//	expectedInfrastructureProviderOrg: cutil.GetPtr(tas[28].InfrastructureProviderOrg),
		//	expectedTenantContact:             ipUser,
		//	expectedAllocationCount:           1,
		//},
		//{
		//	name:                              "success when sort by tenant contact email",
		//	reqOrgName:                        ipOrg1,
		//	user:                              ipUser,
		//	queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
		//	pageNumber:                        cutil.GetPtr(1),
		//	pageSize:                          cutil.GetPtr(10),
		//	orderBy:                           cutil.GetPtr("TENANT_CONTACT_EMAIL_DESC"),
		//	expectedStatus:                    http.StatusOK,
		//	expectedCnt:                       10,
		//	expectedTotal:                     cutil.GetPtr(totalCount / 2),
		//	expectedFirstEntry:                &tas[0],
		//	expectedTenantOrg:                 cutil.GetPtr(tas[0].TenantOrg),
		//	expectedInfrastructureProviderOrg: cutil.GetPtr(tas[0].InfrastructureProviderOrg),
		//	expectedTenantContact:             contactUser1,
		//	expectedAllocationCount:           1,
		//},
		//{
		//	name:                              "success when sort by tenant contact full name",
		//	reqOrgName:                        ipOrg1,
		//	user:                              ipUser,
		//	queryInfrastructureProviderID:     cutil.GetPtr(ip1.ID.String()),
		//	pageNumber:                        cutil.GetPtr(1),
		//	pageSize:                          cutil.GetPtr(10),
		//	orderBy:                           cutil.GetPtr("TENANT_CONTACT_FULL_NAME_DESC"),
		//	expectedStatus:                    http.StatusOK,
		//	expectedCnt:                       10,
		//	expectedTotal:                     cutil.GetPtr(totalCount / 2),
		//	expectedFirstEntry:                &tas[0],
		//	expectedTenantOrg:                 cutil.GetPtr(tas[0].TenantOrg),
		//	expectedInfrastructureProviderOrg: cutil.GetPtr(tas[0].InfrastructureProviderOrg),
		//	expectedTenantContact:             contactUser1,
		//	expectedAllocationCount:           1,
		//},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.queryInfrastructureProviderID != nil {
				q.Add("infrastructureProviderId", *tc.queryInfrastructureProviderID)
			}
			if tc.queryTenantID != nil {
				q.Add("tenantId", *tc.queryTenantID)
			}
			if tc.queryStatus != nil {
				q.Add("status", *tc.queryStatus)
			}
			if tc.querySearchQuery != nil {
				q.Add("query", *tc.querySearchQuery)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
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

			path := fmt.Sprintf("/v2/org/%s/nico/tenant/account?%s", tc.reqOrgName, q.Encode())

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

			gatah := GetAllTenantAccountHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := gatah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			if tc.expectedErr {
				return
			}

			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)

			resp := []model.APITenantAccount{}
			err = json.Unmarshal(rec.Body.Bytes(), &resp)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedCnt, len(resp))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, pr.Total)
			}

			if tc.expectedFirstEntry != nil {
				assert.Equal(t, tc.expectedFirstEntry.ID.String(), resp[0].ID)
				assert.NotNil(t, resp[0].TenantContact)
				assert.Equal(t, *tc.expectedTenantOrg, resp[0].TenantOrg)
				assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProviderOrg)
				assert.Equal(t, tc.expectedTenantContact.ID.String(), resp[0].TenantContact.ID)
				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProvider.Org)
					}

					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, resp[0].Tenant.Org)
					}
				} else {
					// when user requests to sort by tenant org name or display name, cloud-db automatically includes tenant relation in order to execute sort
					if tc.orderBy != nil && !strings.HasPrefix(*tc.orderBy, "TENANT_ORG_NAME") && !strings.HasPrefix(*tc.orderBy, "TENANT_ORG_DISPLAY_NAME") {
						assert.Nil(t, resp[0].Tenant)
					}
					assert.Nil(t, resp[0].InfrastructureProvider)
				}
			}

			for _, apita := range resp {
				assert.Equal(t, 2, len(apita.StatusHistory))
				assert.Equal(t, tc.expectedAllocationCount, apita.AllocationCount)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestTenantAccountHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testTenantAccountInitDB(t)
	defer dbSession.Close()

	testTenantAccountSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"

	ipOrgRoles := []string{authz.ProviderAdminRole}

	ipUser := testTenantAccountBuildUser(t, dbSession, "test123", []string{ipOrg1, ipOrg2, ipOrg3}, ipOrgRoles, "John", "Doe")

	ip1 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 1", ipOrg1, ipUser)
	assert.NotNil(t, ip1)
	ip2 := testTenantAccountBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 2", ipOrg2, ipUser)
	assert.NotNil(t, ip2)

	tn1 := testTenantAccountBuildTenant(t, dbSession, tnOrg1, "Test Tenant Account 1", ipUser)
	assert.NotNil(t, tn1)
	tn2 := testTenantAccountBuildTenant(t, dbSession, tnOrg2, "Test Tenant Account 2", ipUser)
	assert.NotNil(t, tn2)
	tn3 := testTenantAccountBuildTenant(t, dbSession, tnOrg3, "Test Tenant Account 3", ipUser)
	assert.NotNil(t, tn2)
	tn4 := testTenantAccountBuildTenant(t, dbSession, tnOrg3, "Test Tenant Account 4", ipUser)
	assert.NotNil(t, tn2)

	ta1 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip1, tn1, tnOrg1, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta1)
	testTenantAccountBuildStatusDetail(t, dbSession, ta1.ID, ta1.Status)
	ta2 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip1, tn2, tnOrg2, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta2)
	testTenantAccountBuildStatusDetail(t, dbSession, ta2.ID, ta2.Status)

	site := testTenantAccountBuildSite(t, dbSession, ip2, "Test Site", ipUser)
	assert.NotNil(t, site)
	allocation := testTenantAccountBuildAllocation(t, dbSession, site, tn3, "Test Allocation", ipUser)
	assert.NotNil(t, allocation)

	ta3 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip2, tn3, tnOrg3, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta3)
	testTenantAccountBuildStatusDetail(t, dbSession, ta3.ID, ta3.Status)

	ta4 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip2, tn4, tnOrg4, cdbm.TenantAccountStatusInvited, ipUser.ID, uuid.Nil)
	assert.NotNil(t, ta4)
	testTenantAccountBuildStatusDetail(t, dbSession, ta4.ID, ta4.Status)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		taID               string
		expectedErr        bool
		expectedStatus     int
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			taID:           ta1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when tenantaccount id is invalid uuid",
			reqOrgName:     ipOrg1,
			user:           ipUser,
			taID:           "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have infrastructure provider",
			reqOrgName:     ipOrg3,
			user:           ipUser,
			taID:           ta1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified tenant account doesnt exist",
			reqOrgName:     ipOrg1,
			user:           ipUser,
			taID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when org's infrastructure provider does not match tenantaccount's infrastructure provider",
			reqOrgName:     ipOrg1,
			user:           ipUser,
			taID:           ta3.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when allocations exist for tenant",
			reqOrgName:     ipOrg2,
			user:           ipUser,
			taID:           ta3.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case 1",
			reqOrgName:         ipOrg1,
			user:               ipUser,
			taID:               ta1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name:           "success case 2",
			reqOrgName:     ipOrg1,
			user:           ipUser,
			taID:           ta2.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusAccepted,
		},
		{
			name:           "success case 3, Tenant Account was never accepted by Tenant",
			reqOrgName:     ipOrg2,
			user:           ipUser,
			taID:           ta4.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusAccepted,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.taID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := DeleteTenantAccountHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr && rec.Code != http.StatusAccepted {
				rsp := &model.APITenantAccount{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
