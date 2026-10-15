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
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
)

func testSSHKeySetupSchema(t *testing.T, dbSession *cdb.Session) {
	testInstanceSetupSchema(t, dbSession)

	// create SSHKeyGroup table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroup)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroupSiteAssociation
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)

	// create SSHKey table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKey)(nil))
	assert.Nil(t, err)
	// create SSHKeyAssociation
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyAssociation)(nil))
	assert.Nil(t, err)
}

func testBuildSSHKey(t *testing.T, dbSession *cdb.Session, name, org string, tenantID uuid.UUID, publicKey string, fingerprint *string, expires *time.Time, createdBy uuid.UUID) *cdbm.SSHKey {
	sshkey := &cdbm.SSHKey{
		ID:          uuid.New(),
		Name:        name,
		Org:         org,
		TenantID:    tenantID,
		PublicKey:   publicKey,
		Fingerprint: fingerprint,
		Expires:     expires,
		CreatedBy:   createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshkey).Exec(context.Background())
	assert.Nil(t, err)
	return sshkey
}

func testBuildSSHKeyAssociation(t *testing.T, dbSession *cdb.Session, sshKeyID uuid.UUID, sshKeyGroupID uuid.UUID, createdBy uuid.UUID) *cdbm.SSHKeyAssociation {
	sshkeyassociation := &cdbm.SSHKeyAssociation{
		ID:            uuid.New(),
		SSHKeyID:      sshKeyID,
		SSHKeyGroupID: sshKeyGroupID,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshkeyassociation).Exec(context.Background())
	assert.Nil(t, err)
	return sshkeyassociation
}

func TestSSHKeyHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	tnu2Bad := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	skg1 := testBuildSSHKeyGroup(t, dbSession, "sre-ssh-group-1", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg1)

	skg2 := testBuildSSHKeyGroup(t, dbSession, "sre-ssh-group-2", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusDeleting, tnu1.ID)
	assert.NotNil(t, skg2)

	skg3 := testBuildSSHKeyGroup(t, dbSession, "sre-ssh-group-3", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg3)

	skgsa3 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg3.ID, st1.ID, nil, cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa3)

	goodPublicKey1 := "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICip4hl6WjuVHs60PeikVUs0sWE/kPhk2D0rRHWsIuyL jdoe@test.com"
	goodPublicKeyRSA1 := "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDBlrwxTSSkPGrJ5nfJsx1AXHP/JCLMO1ukrSJqZThZ+CHrk5l2UeRFc0eD6RM+DoQ/YWoSoFItMIV8SdcDBKfG4lXrMLkr13IyBZ5c6RaEn9a4BEhhfFzWuXJoxnvPSSOboiuhYNDa58oj0Qp+TYX475NDBoE48ZmHA8RWPirD7KgzAgsq3Tdj1CZG60Zy2ff/mpcpNkmoU0KuVhtIy0eqXtv3GQmuKKiJf8GNIMZ7gjzZsQkSmYmpUmCQbQGQ7VzuRLVjUElcI8oAedIWSkk7fWoJBBd1jAGr4ATxbf0ltzcVvnCeNmU3f6n1sjKIuKavPRQnD3IntR2O38RgSOuv jdoe1@test1.com"
	goodPublicKeyRSA2 := "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCKAzJrQfiWS2z/fOfDkSromUi7wv9KYwtL4IROE8hBoF8KRPysSzIglcqE65+xjJP1utFFh3YXgpe+MQMeRTshVXyqqGo04nNAO+/XvgakAdPH2w6zC30Yd+Ex4AbeJkvV0NfVZdOad52W3LnDic5t1dyhcam4Ig8o97RH919Ih08RGcewKNF46WQODJr7SdA3o0/iPVHatkKmU2HNEx2gVbVMyttn4iuYmm12UeN7KESFEkHO5Ayu5hJS74mBwvytQH5iz63G7lVIa2bGPpK8/korRS/++gMl0oncFQYJ07FlInDhlT+BmotpMtWvHWI5Ajf+3rOfSSQ6w/whDh05"

	okBody, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok1", PublicKey: goodPublicKey1})
	assert.Nil(t, err)
	errBodyNameClash, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok1", PublicKey: goodPublicKey1})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok2", PublicKey: goodPublicKeyRSA1})
	assert.Nil(t, err)
	okBodyWithSSHKeyGroup, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok3", PublicKey: goodPublicKeyRSA2, SSHKeyGroupID: cutil.GetPtr(skg1.ID.String())})
	assert.Nil(t, err)
	okBodyWithBadSSHKeyGroup, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok4", PublicKey: goodPublicKeyRSA2, SSHKeyGroupID: cutil.GetPtr("test")})
	assert.Nil(t, err)
	errBodyPublicKeyConflict, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok5", PublicKey: goodPublicKey1})
	assert.Nil(t, err)
	okBodyWithDeletingSSHKeyGroup, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok6", PublicKey: goodPublicKeyRSA2, SSHKeyGroupID: cutil.GetPtr(skg2.ID.String())})
	assert.Nil(t, err)
	goodPublicKeyRSA3 := "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCBlrwxTSSkPGrJ5nfJsx1AXHP/JCLMO1ukrSJqZThZ+CHrk5l2UeRFc0eD6RM+DoQ/YWoSoFItMIV8SdcDBKfG4lXrMLkr13IyBZ5c6RaEn9a4BEhhfFzWuXJoxnvPSSOboiuhYNDa58oj0Qp+TYX475NDBoE48ZmHA8RWPirD7KgzAgsq3Tdj1CZG60Zy2ff/mpcpNkmoU0KuVhtIy0eqXtv3GQmuKKiJf8GNIMZ7gjzZsQkSmYmpUmCQbQGQ7VzuRLVjUElcI8oAedIWSkk7fWoJBBd1jAGr4ATxbf0ltzcVvnCeNmU3f6n1sjKIuKavPRQnD3IntR2O38RgSOuv jdoe3@test.com"
	okBodyWithSSHKeyGroupWithSite, err := json.Marshal(model.APISSHKeyCreateRequest{Name: "ok7", PublicKey: goodPublicKeyRSA3, SSHKeyGroupID: cutil.GetPtr(skg3.ID.String())})
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                      string
		reqOrgName                string
		reqBody                   string
		reqSSHKeyGroupID          uuid.UUID
		user                      *cdbm.User
		expectedErr               bool
		expectedStatus            int
		expectedSSHKeyGroupStatus string
		expectedSSHKeyGroup       bool
		verifyChildSpanner        bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			reqBody:        string(okBody),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			reqBody:        string(okBody),
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant role is not admin forbidden",
			reqOrgName:     tnOrg,
			reqBody:        string(okBody),
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body doesnt bind",
			reqOrgName:     tnOrg,
			reqBody:        "SomeNonJsonBody",
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when request doesnt validate",
			reqOrgName:     tnOrg,
			reqBody:        string(errBodyDoesntValidate),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant not in org",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody),
			user:           tnu2Bad,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case edcsa key",
			reqOrgName:         tnOrg,
			reqBody:            string(okBody),
			user:               tnu1,
			expectedErr:        false,
			expectedStatus:     http.StatusCreated,
			verifyChildSpanner: true,
		},
		{
			name:           "error when name clashes",
			reqOrgName:     tnOrg,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "error when bad sshkeygroup id",
			reqOrgName:     tnOrg,
			reqBody:        string(okBodyWithBadSSHKeyGroup),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when sshkeygroup is in deleting state",
			reqOrgName:     tnOrg,
			reqBody:        string(okBodyWithDeletingSSHKeyGroup),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "success case rsa key",
			reqOrgName:     tnOrg,
			reqBody:        string(okBody2),
			user:           tnu1,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
		},
		{
			name:                      "success case sshkeygroup specified with no site associations, status unchanged",
			reqOrgName:                tnOrg,
			reqBody:                   string(okBodyWithSSHKeyGroup),
			reqSSHKeyGroupID:          skg1.ID,
			user:                      tnu1,
			expectedErr:               false,
			expectedStatus:            http.StatusCreated,
			expectedSSHKeyGroup:       true,
			expectedSSHKeyGroupStatus: cdbm.SSHKeyGroupStatusSynced,
		},
		{
			name:                      "success case sshkeygroup specified with a site association, status set to syncing",
			reqOrgName:                tnOrg,
			reqBody:                   string(okBodyWithSSHKeyGroupWithSite),
			reqSSHKeyGroupID:          skg3.ID,
			user:                      tnu1,
			expectedErr:               false,
			expectedStatus:            http.StatusCreated,
			expectedSSHKeyGroup:       true,
			expectedSSHKeyGroupStatus: cdbm.SSHKeyGroupStatusSyncing,
		},
		{
			name:           "error when pubic key clashes",
			reqOrgName:     tnOrg,
			reqBody:        string(errBodyPublicKeyConflict),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
	}

	tmc := &tmocks.Client{}
	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	// Mock sync call
	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID, string) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID"), mock.AnythingOfType("string")).Return(wrun, nil)

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

			csgh := CreateSSHKeyHandler{
				dbSession: dbSession,
				tc:        tmc,
				cfg:       cfg,
			}

			err := csgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code)

			if !tc.expectedErr {
				rsp := &model.APISSHKey{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)

				// validate response fields
				if tc.expectedSSHKeyGroup {
					skgDAO := cdbm.NewSSHKeyGroupDAO(dbSession)
					skg, err := skgDAO.GetByID(ctx, nil, tc.reqSSHKeyGroupID, nil)
					assert.Nil(t, err)
					assert.Equal(t, tc.expectedSSHKeyGroupStatus, skg.Status)
					assert.NotNil(t, skg.Version)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyHandler_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)
	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu2 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	sk1 := testBuildSSHKey(t, dbSession, "test", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSyncing, tnu1.ID)

	ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska1)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                   string
		reqOrgName             string
		user                   *cdbm.User
		skID                   string
		expectedErr            bool
		expectedStatus         int
		expectedTenantOrg      *string
		queryIncludeRelations1 *string
		verifyChildSpanner     bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant not in org",
			reqOrgName:     tnOrg2,
			user:           tnu1,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when sk id is not uuid",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skID:           "badid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when sk not found",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when sk tenant doesnt match tenant in org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "success case",
			reqOrgName:         tnOrg,
			user:               tnu1,
			skID:               sk1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name:                   "success case when include relation",
			reqOrgName:             tnOrg,
			user:                   tnu1,
			skID:                   sk1.ID.String(),
			queryIncludeRelations1: cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedTenantOrg:      cutil.GetPtr(tn1.Org),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}

			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.skID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			dsgh := GetSSHKeyHandler{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			}

			err := dsgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISSHKey{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.skID, rsp.ID)
				if tc.queryIncludeRelations1 != nil {
					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
				} else {
					assert.Nil(t, rsp.Tenant)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testSSHKeyGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)
	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	sks := []*cdbm.SSHKey{}
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSyncing, tnu1.ID)
	skg2 := testBuildSSHKeyGroup(t, dbSession, "test2", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSyncing, tnu1.ID)

	sshKeySkg1 := []*cdbm.SSHKey{}
	sshKeySkg2 := []*cdbm.SSHKey{}

	totalKeys := 25
	for i := 1; i <= totalKeys; i++ {
		sk1 := testBuildSSHKey(t, dbSession, fmt.Sprintf("test-%d", i), tnOrg, tn1.ID, fmt.Sprintf("testpublickey-%d", i), cutil.GetPtr("test"), nil, tnu1.ID)

		if i <= 10 {
			ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
			assert.NotNil(t, ska1)
			sshKeySkg1 = append(sshKeySkg1, sk1)
		} else {
			ska2 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg2.ID, tnu1.ID)
			assert.NotNil(t, ska2)
			sshKeySkg2 = append(sshKeySkg2, sk1)
		}
		sks = append(sks, sk1)
	}

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                   string
		reqOrgName             string
		qSshKeyGroupID         *string
		querySearch            *string
		queryIncludeRelations1 *string
		pageNumber             *int
		pageSize               *int
		orderBy                *string
		user                   *cdbm.User
		expectedErr            bool
		expectedStatus         int
		expectedSkCnt          int
		expectedTotal          *int
		expectedFirstSkName    *string
		expectedTenantOrg      *string
		verifyChildSpanner     bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant not in org",
			reqOrgName:     tnOrg2,
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                "success case with objects returned",
			reqOrgName:          tnOrg,
			user:                tnu1,
			orderBy:             cutil.GetPtr("CREATED_ASC"),
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedSkCnt:       20,
			expectedTotal:       cutil.GetPtr(25),
			expectedFirstSkName: cutil.GetPtr("test-1"),
			verifyChildSpanner:  true,
		},
		{
			name:                "success case filter by sshkeygroupid",
			reqOrgName:          tnOrg,
			user:                tnu1,
			qSshKeyGroupID:      cutil.GetPtr(skg2.ID.String()),
			orderBy:             cutil.GetPtr("CREATED_ASC"),
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedSkCnt:       len(sshKeySkg2),
			expectedTotal:       cutil.GetPtr(len(sshKeySkg2)),
			expectedFirstSkName: cutil.GetPtr("test-11"),
			verifyChildSpanner:  true,
		},
		{
			name:                "success case, with paging",
			reqOrgName:          tnOrg,
			user:                tnu1,
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			pageNumber:          cutil.GetPtr(1),
			pageSize:            cutil.GetPtr(10),
			orderBy:             cutil.GetPtr("CREATED_ASC"),
			expectedSkCnt:       10,
			expectedTotal:       cutil.GetPtr(25),
			expectedFirstSkName: cutil.GetPtr("test-1"),
		},
		{
			name:                "success case, with paging 2",
			reqOrgName:          tnOrg,
			user:                tnu1,
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			pageNumber:          cutil.GetPtr(2),
			pageSize:            cutil.GetPtr(10),
			orderBy:             cutil.GetPtr("CREATED_ASC"),
			expectedSkCnt:       10,
			expectedTotal:       cutil.GetPtr(25),
			expectedFirstSkName: cutil.GetPtr("test-11"),
		},
		{
			name:           "error case, with paging",
			reqOrgName:     tnOrg,
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedSkCnt:  0,
		},
		{
			name:                   "success case with include relation",
			reqOrgName:             tnOrg,
			user:                   tnu1,
			orderBy:                cutil.GetPtr("CREATED_ASC"),
			queryIncludeRelations1: cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedSkCnt:          20,
			expectedTotal:          cutil.GetPtr(25),
			expectedFirstSkName:    cutil.GetPtr("test-1"),
			expectedTenantOrg:      cutil.GetPtr(tn1.Org),
		},
		{
			name:           "success case with name search query",
			reqOrgName:     tnOrg,
			user:           tnu1,
			querySearch:    cutil.GetPtr("test"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedSkCnt:  20,
			expectedTotal:  cutil.GetPtr(25),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tc.pageNumber))
			}
			if tc.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tc.pageSize))
			}
			if tc.orderBy != nil {
				q.Set("orderBy", *tc.orderBy)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.qSshKeyGroupID != nil {
				q.Add("sshKeyGroupId", *tc.qSshKeyGroupID)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/sshkey?%s", tc.reqOrgName, q.Encode())

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

			gsgh := GetAllSSHKeyHandler{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			}
			err := gsgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if tc.expectedErr {
				return
			}

			rsp := []model.APISSHKey{}
			err = json.Unmarshal(rec.Body.Bytes(), &rsp)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedSkCnt, len(rsp))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, pr.Total)
			}

			if tc.expectedFirstSkName != nil {
				assert.Equal(t, *tc.expectedFirstSkName, (rsp)[0].Name)
			}
			if tc.queryIncludeRelations1 != nil {
				if tc.expectedTenantOrg != nil {
					assert.Equal(t, *tc.expectedTenantOrg, rsp[0].Tenant.Org)
				}
			} else {
				if len(rsp) > 0 {
					assert.Nil(t, rsp[0].Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)
	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu2 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	assert.NotNil(t, tn1)

	sk1 := testBuildSSHKey(t, dbSession, "test-ssh-key-1", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	assert.NotNil(t, sk1)
	sk2 := testBuildSSHKey(t, dbSession, "test-ssh-key-2", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	assert.NotNil(t, sk2)

	errBody1, err := json.Marshal(model.APISSHKeyUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(model.APISSHKeyUpdateRequest{Name: cutil.GetPtr("test-ssh-key-2")})
	assert.Nil(t, err)

	okBody, err := json.Marshal(model.APISSHKeyUpdateRequest{Name: cutil.GetPtr("test-ssh-key-updated")})
	assert.Nil(t, err)

	okBody2, err := json.Marshal(model.APISSHKeyUpdateRequest{Name: cutil.GetPtr("test-ssh-key-1")})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()
	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		skID               string
		reqBody            string
		expectedErr        bool
		expectedStatus     int
		expectedName       *string
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant not in org",
			reqOrgName:     tnOrg2,
			user:           tnu1,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when sk id is not uuid",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skID:           "badid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when sk not found",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when sk tenant doesnt match tenant in org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when body does not validate",
			reqOrgName:     tnOrg,
			reqBody:        string(errBody1),
			user:           tnu1,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when body does not bind",
			reqOrgName:     tnOrg,
			reqBody:        "badbody",
			user:           tnu1,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when name clashes",
			reqOrgName:     tnOrg,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			skID:           sk1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:               "success case when name is updated",
			reqOrgName:         tnOrg,
			reqBody:            string(okBody),
			user:               tnu1,
			skID:               sk1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name:           "success case when name is same",
			reqOrgName:     tnOrg,
			reqBody:        string(okBody2),
			user:           tnu1,
			skID:           sk1.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.skID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			usgh := UpdateSSHKeyHandler{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			}
			err := usgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISSHKey{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.skID, rsp.ID)
				if tc.expectedName != nil {
					assert.Equal(t, *tc.expectedName, rsp.Name)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	tnu2Bad := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	// Build SSHKeyGroup1
	sk1 := testBuildSSHKey(t, dbSession, "test", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	sk2 := testBuildSSHKey(t, dbSession, "test-2", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)

	skg1 := testBuildSSHKeyGroup(t, dbSession, "test", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSynced, tnu1.ID)

	ska11 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska11)
	ska12 := testBuildSSHKeyAssociation(t, dbSession, sk2.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska12)

	cfg := common.GetTestConfig()

	tmc := &tmocks.Client{}
	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	// Mock sync call
	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID, string) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID"), mock.AnythingOfType("string")).Return(wrun, nil)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                      string
		reqOrgName                string
		user                      *cdbm.User
		skID                      string
		skgID                     *uuid.UUID
		expectedErr               bool
		expectedStatus            int
		expectedSSHKeyGroupStatus *string
		verifyChildSpanner        bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user role is forbidden",
			reqOrgName:     tnOrg,
			user:           tnu1Forbidden,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant not in org",
			reqOrgName:     tnOrg2,
			user:           tnu2Bad,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when sk id is not uuid",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skID:           "badid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when sk not found",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:                      "success case",
			reqOrgName:                tnOrg,
			user:                      tnu1,
			skID:                      sk1.ID.String(),
			skgID:                     &skg1.ID,
			expectedErr:               false,
			expectedStatus:            http.StatusAccepted,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
			verifyChildSpanner:        true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.skID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			dsgh := DeleteSSHKeyHandler{
				dbSession: dbSession,
				tc:        tmc,
				cfg:       cfg,
			}

			err := dsgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				// verify sshkey deleted
				sgDAO := cdbm.NewSSHKeyDAO(dbSession)
				id1, err := uuid.Parse(tc.skID)
				assert.Nil(t, err)
				_, err = sgDAO.GetByID(ctx, nil, id1, nil)
				assert.NotNil(t, err)
				// verify that the public key is cleared out in the deleted sk
				sk := &cdbm.SSHKey{}
				err = cdb.GetIDB(nil, dbSession).NewSelect().Model(sk).WhereAllWithDeleted().Where("sk.id = ?", tc.skID).Scan(ctx)
				assert.Nil(t, err)
				assert.Equal(t, "", sk.PublicKey)
				// verify sk associations
				skaDAO := cdbm.NewSSHKeyAssociationDAO(dbSession)
				_, tot, err := skaDAO.GetAll(ctx, nil, cdbm.SSHKeyAssociationFilterInput{
					SSHKeyIDs: []uuid.UUID{sk1.ID},
				}, cdbp.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 0, tot)

				// verify skg status
				if tc.expectedSSHKeyGroupStatus != nil && tc.skgID != nil {
					skgDAO := cdbm.NewSSHKeyGroupDAO(dbSession)
					skg, err := skgDAO.GetByID(ctx, nil, *tc.skgID, nil)
					assert.Nil(t, err)
					assert.Equal(t, *tc.expectedSSHKeyGroupStatus, skg.Status)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
