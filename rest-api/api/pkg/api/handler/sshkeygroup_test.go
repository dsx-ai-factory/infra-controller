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
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
)

func testSSHKeyGroupSetupSchema(t *testing.T, dbSession *cdb.Session) {
	testSiteSetupSchema(t, dbSession)
	// create SSHKeyGroupSiteAssociation
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroup)(nil))
	assert.Nil(t, err)
	// create SSHKey table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKey)(nil))
	assert.Nil(t, err)
	// create SSHKeyAssociation
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyAssociation)(nil))
	assert.Nil(t, err)

}

func testBuildSSHKeyGroup(t *testing.T, dbSession *cdb.Session, name, org string, description *string, tenantID uuid.UUID, version *string, status string, createdBy uuid.UUID) *cdbm.SSHKeyGroup {
	sshkeygroup := &cdbm.SSHKeyGroup{
		ID:          uuid.New(),
		Name:        name,
		Org:         org,
		Description: description,
		TenantID:    tenantID,
		Version:     version,
		Status:      status,
		CreatedBy:   createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshkeygroup).Exec(context.Background())
	assert.Nil(t, err)
	return sshkeygroup
}

func testBuildSSHKeyGroupSiteAssociation(t *testing.T, dbSession *cdb.Session, sshKeyGroupID uuid.UUID, siteID uuid.UUID, version *string, status string, createdBy uuid.UUID) *cdbm.SSHKeyGroupSiteAssociation {
	SSHKeyGroupSiteAssociation := &cdbm.SSHKeyGroupSiteAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: sshKeyGroupID,
		SiteID:        siteID,
		Version:       version,
		Status:        status,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(SSHKeyGroupSiteAssociation).Exec(context.Background())
	assert.Nil(t, err)
	return SSHKeyGroupSiteAssociation
}

func testBuildTenantSiteAssociation(t *testing.T, dbSession *cdb.Session, org string, tenantID uuid.UUID, siteID uuid.UUID, createdBy uuid.UUID) *cdbm.TenantSite {
	tenantsiteassociation := &cdbm.TenantSite{
		ID:                  uuid.New(),
		TenantID:            tenantID,
		SiteID:              siteID,
		EnableSerialConsole: false,
		CreatedBy:           createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(tenantsiteassociation).Exec(context.Background())
	assert.Nil(t, err)
	return tenantsiteassociation
}

func TestSSHKeyGroupHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testSSHKeyGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Tenant 1
	tnu1 := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tnu2Bad := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	skg1 := testBuildSSHKeyGroup(t, dbSession, "sre-ssh-group", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg1)

	st1 := testVPCBuildSite(t, dbSession, ip, "test-site-1", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st1)

	st2 := testVPCBuildSite(t, dbSession, ip, "test-site-2", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st2)

	sk1 := testBuildSSHKey(t, dbSession, "test1", tnOrg, tn1.ID, "testpublickey1", cutil.GetPtr("test1"), nil, tnu1.ID)
	assert.NotNil(t, sk1)

	sk2 := testBuildSSHKey(t, dbSession, "test2", tnOrg, tn1.ID, "testpublickey2", cutil.GetPtr("test2"), nil, tnu1.ID)
	assert.NotNil(t, sk2)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	okBody, err := json.Marshal(model.APISSHKeyGroupCreateRequest{Name: "ok1", Description: cutil.GetPtr("test"),
		SiteIDs: []string{st1.ID.String(), st2.ID.String()}, SSHKeyIDs: []string{sk1.ID.String()}})
	assert.Nil(t, err)

	okBody2, err := json.Marshal(model.APISSHKeyGroupCreateRequest{Name: "ok2", Description: cutil.GetPtr("test"), SiteIDs: []string{}, SSHKeyIDs: []string{sk1.ID.String()}})
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(model.APISSHKeyGroupCreateRequest{Name: "ok1"})
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ TName string }{TName: "test"})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()

	tmc := &tmocks.Client{}
	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID, string) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID"), mock.AnythingOfType("string")).Return(wrun, nil)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                     string
		reqOrgName               string
		reqBody                  string
		user                     *cdbm.User
		expectedErr              bool
		expectedStatus           int
		expectedSiteAssociations bool
		countSiteAssociations    int
		expectedSSHKeys          bool
		countSSHKeys             int
		expectVersion            bool
		expectSSHKeyGroupStatus  *string
		verifyChildSpanner       bool
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
			name:                     "success case creating SSH Key Group with Site associations",
			reqOrgName:               tnOrg,
			reqBody:                  string(okBody),
			user:                     tnu1,
			expectedErr:              false,
			expectVersion:            true,
			expectedSiteAssociations: true,
			expectedSSHKeys:          true,
			expectedStatus:           http.StatusCreated,
			countSiteAssociations:    2,
			countSSHKeys:             1,
			expectSSHKeyGroupStatus:  cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
		},
		{
			name:                     "success case creating SSH Key Group without Site associations",
			reqOrgName:               tnOrg,
			reqBody:                  string(okBody2),
			user:                     tnu1,
			expectedErr:              false,
			expectVersion:            true,
			expectedSiteAssociations: false,
			expectedSSHKeys:          true,
			expectedStatus:           http.StatusCreated,
			countSiteAssociations:    0,
			countSSHKeys:             1,
			expectSSHKeyGroupStatus:  cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
		},
		{
			name:           "error when name clashes",
			reqOrgName:     tnOrg,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
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

			csgh := CreateSSHKeyGroupHandler{
				dbSession: dbSession,
				tc:        tmc,
				cfg:       cfg,
			}

			err := csgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISSHKeyGroup{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, tc.countSiteAssociations, len(rsp.SiteAssociations))
				assert.Equal(t, tc.countSSHKeys, len(rsp.SSHKeys))

				if tc.expectVersion {
					assert.NotNil(t, rsp.Version)
				}
				if tc.expectSSHKeyGroupStatus != nil {
					assert.Equal(t, *tc.expectSSHKeyGroupStatus, rsp.Status)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testSSHKeyGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Tenant 1
	tnu1 := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testVPCBuildTenant(t, dbSession, "test-tenant-1", tnOrg, tnu1)

	// Tenant 2
	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles)
	tn2 := testVPCBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	// Sites
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	ts21 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st2.ID, tnu2.ID)
	assert.NotNil(t, ts21)

	st3 := testInstanceBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st3)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st3.ID, tnu1.ID)
	assert.NotNil(t, ts3)

	ts31 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st3.ID, tnu2.ID)
	assert.NotNil(t, ts31)

	st4 := testInstanceBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st4)

	ts4 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st4.ID, tnu1.ID)
	assert.NotNil(t, ts4)

	ts41 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tn2.ID, st3.ID, tnu2.ID)
	assert.NotNil(t, ts41)

	st5 := testInstanceBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st5)

	ts5 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st5.ID, tnu1.ID)
	assert.NotNil(t, ts5)

	ts51 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tn2.ID, st5.ID, tnu2.ID)
	assert.NotNil(t, ts51)

	// SSHKeys
	sk1 := testBuildSSHKey(t, dbSession, "test-ssh-key-1", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	assert.NotNil(t, sk1)

	sk2 := testBuildSSHKey(t, dbSession, "test-ssh-key-2", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	assert.NotNil(t, sk2)

	sk3 := testBuildSSHKey(t, dbSession, "test-ssh-key-3", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	assert.NotNil(t, sk3)

	sk4 := testBuildSSHKey(t, dbSession, "test-ssh-key-4", tnOrg, tn2.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu2.ID)
	assert.NotNil(t, sk4)

	sk5 := testBuildSSHKey(t, dbSession, "test-ssh-key-5", tnOrg, tn2.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu2.ID)
	assert.NotNil(t, sk5)

	sk6 := testBuildSSHKey(t, dbSession, "test-ssh-key-6", tnOrg2, tn2.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu2.ID)
	assert.NotNil(t, sk6)

	sk7 := testBuildSSHKey(t, dbSession, "test-ssh-key-7", tnOrg2, tn2.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu2.ID)
	assert.NotNil(t, sk7)

	// Build SSHKeyGroup 1
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation 1
	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa1)

	// Build SSHKeyAssociation 1
	ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska1)

	// Build SSHKeyGroup 2
	skg2 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-2", tnOrg, cutil.GetPtr("test"), tn2.ID, cutil.GetPtr("124345"), cdbm.SSHKeyGroupStatusSynced, tnu2.ID)
	assert.NotNil(t, skg2)

	// Build SSHKeyGroupSiteAssociation 2
	skgsa2 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg2.ID, st3.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu2.ID)
	assert.NotNil(t, skgsa2)

	// Build SSHKeyAssociation 2
	ska2 := testBuildSSHKeyAssociation(t, dbSession, sk4.ID, skg2.ID, tnu2.ID)
	assert.NotNil(t, ska2)

	// Build SSHKeyGroup 3
	skg3 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-3", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("124745"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg3)

	// Build SSHKeyGroup 4
	skg4 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-4", tnOrg2, cutil.GetPtr("test"), tn2.ID, cutil.GetPtr("131417"), cdbm.SSHKeyGroupStatusSynced, tnu2.ID)
	assert.NotNil(t, skg4)

	// Build SSHKeyGroupSiteAssociation 4
	skgsa4 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg4.ID, st4.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu2.ID)
	assert.NotNil(t, skgsa4)

	// Build SSHKeyAssociation 4
	ska4 := testBuildSSHKeyAssociation(t, dbSession, sk6.ID, skg4.ID, tnu2.ID)
	assert.NotNil(t, ska4)

	// Build SSHKeyGroup 5
	skg5 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-5", tnOrg2, cutil.GetPtr("test"), tn2.ID, cutil.GetPtr("158790"), cdbm.SSHKeyGroupStatusSynced, tnu2.ID)
	assert.NotNil(t, skg5)

	// Build SSHKeyGroupSiteAssociation 5
	skgsa5 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg5.ID, st5.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu2.ID)
	assert.NotNil(t, skgsa5)

	// Build SSHKeyAssociation 5
	ska5 := testBuildSSHKeyAssociation(t, dbSession, sk6.ID, skg5.ID, tnu2.ID)
	assert.NotNil(t, ska5)

	// Build SSHKeyGroup 6
	skg6 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-6", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345678"), cdbm.SSHKeyGroupStatusDeleting, tnu1.ID)
	assert.NotNil(t, skg6)

	// Build SSHKeyGroup 7
	skg7 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-7", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345678"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg7)

	// Add 2 keys but no Site Association
	ska71 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg7.ID, tnu1.ID)
	assert.NotNil(t, ska71)

	ska72 := testBuildSSHKeyAssociation(t, dbSession, sk2.ID, skg7.ID, tnu1.ID)
	assert.NotNil(t, ska72)

	// Build SSHKeyGroup 8
	skg8 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-8", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345678"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg8)

	// Add a Site Association but no keys
	skgsa81 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg8.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa81)

	// Build SSHKeyGroup 9
	skg9 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-9", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345678"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg9)

	// Add a Site Association and a Key Association
	skgsa91 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg9.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa91)

	ska91 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg9.ID, tnu1.ID)
	assert.NotNil(t, ska91)

	// Build SSHKeyGroup 10
	skg10 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-10", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345678"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg10)

	// Build SSHKeyGroup 11
	skg11 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-10", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345678"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg11)

	// Add a Key Association
	ska111 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg11.ID, tnu1.ID)
	assert.NotNil(t, ska111)

	// Populate request data
	errBody1, err := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	errBodyVersionMatch, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("test-sshkeygroup-2")})
	assert.NotNil(t, errBodyVersionMatch)

	errBodyNameClash, err := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("test-sshkeygroup-3"), Version: skg1.Version})
	assert.Nil(t, err)

	// only name update
	okNameUpdateBody1, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("test-sshkeygroup-updated"), Version: skg1.Version})
	assert.NotNil(t, okNameUpdateBody1)

	okNameUpdateBody2, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("test-sshkeygroup-updated-new"), Version: skg5.Version})
	assert.NotNil(t, okNameUpdateBody2)

	okSiteKeyUpdateBody1, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("test-sshkeygroup-updated-1"), Description: cutil.GetPtr("testdescription"), Version: skg1.Version, SiteIDs: []string{st1.ID.String(), st2.ID.String()}, SSHKeyIDs: []string{sk1.ID.String(), sk2.ID.String()}})
	assert.NotNil(t, okSiteKeyUpdateBody1)

	okSiteKeyUpdateBody2, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("test-sshkeygroup-updated-2"), Description: cutil.GetPtr("testdescription"), Version: skg2.Version, SiteIDs: []string{st2.ID.String()}, SSHKeyIDs: []string{sk5.ID.String()}})
	assert.NotNil(t, okSiteKeyUpdateBody2)

	okDeleteAllSiteKeyBody1, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Version: skg4.Version, SiteIDs: []string{}, SSHKeyIDs: []string{}})
	assert.NotNil(t, okDeleteAllSiteKeyBody1)

	okKeyUpdateBody7, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Version: skg7.Version, SSHKeyIDs: []string{sk2.ID.String(), sk3.ID.String()}})
	assert.NotNil(t, okKeyUpdateBody7)

	okSiteUpdateBody8, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Version: skg8.Version, SiteIDs: []string{st1.ID.String(), st2.ID.String()}})
	assert.NotNil(t, okSiteUpdateBody8)

	okSiteKeyUpdateBody9, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Version: skg8.Version, SiteIDs: []string{st1.ID.String()}, SSHKeyIDs: []string{sk1.ID.String()}})
	assert.NotNil(t, okSiteUpdateBody8)

	okSiteKeyUpdateBody10, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Version: skg6.Version, SiteIDs: []string{st1.ID.String()}, SSHKeyIDs: []string{sk1.ID.String()}})
	assert.NotNil(t, okSiteKeyUpdateBody10)

	okSiteKeyUpdateBody11, _ := json.Marshal(model.APISSHKeyGroupUpdateRequest{Version: skg11.Version, SiteIDs: []string{}, SSHKeyIDs: []string{}})
	assert.NotNil(t, okSiteKeyUpdateBody11)

	cfg := common.GetTestConfig()

	tmc := &tmocks.Client{}
	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	// Mock sync call
	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID, string) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID"), mock.AnythingOfType("string")).Return(wrun, nil)

	// Mock delete call
	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                                       string
		reqOrgName                                 string
		user                                       *cdbm.User
		skgID                                      string
		reqBody                                    string
		expectedErr                                bool
		expectedStatus                             int
		expectedName                               *string
		expectedSiteAssociations                   bool
		countSiteAssociations                      int
		expectedSSHKeys                            bool
		countSSHKeys                               int
		expectedDeletingSSHKeyGroupSiteAssociation bool
		DeletingSSHKeyGroupSiteAssociationID       uuid.UUID
		expectVersion                              *string
		expectedSSHKeyGroupName                    *string
		expectedSSHKeyGroupStatus                  *string
		verifyChildSpanner                         bool
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
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when skg id is not uuid",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skgID:          "badid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when skg not found",
			reqOrgName:     tnOrg,
			user:           tnu1,
			reqBody:        string(okNameUpdateBody1),
			skgID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when skg tenant doesnt match tenant in org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			reqBody:        string(okNameUpdateBody1),
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when body does not validate, version missing",
			reqOrgName:     tnOrg,
			reqBody:        string(errBody1),
			user:           tnu1,
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when body does not bind",
			reqOrgName:     tnOrg,
			reqBody:        "badbody",
			user:           tnu1,
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when name clashes",
			reqOrgName:     tnOrg,
			reqBody:        string(errBodyNameClash),
			user:           tnu1,
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "error when sshkey group is in deleteing state",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skgID:          skg6.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:                      "success case when only name updated, sites and keys are nil in the request",
			reqOrgName:                tnOrg2,
			reqBody:                   string(okNameUpdateBody2),
			user:                      tnu2,
			skgID:                     skg5.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			verifyChildSpanner:        true,
			expectedName:              cutil.GetPtr("test-sshkeygroup-updated-new"),
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
			expectedSiteAssociations:  true,
			expectedSSHKeys:           true,
			countSSHKeys:              1,
			countSiteAssociations:     1,
		},
		{
			name:                      "success case when name, site and keys updated",
			reqOrgName:                tnOrg,
			reqBody:                   string(okSiteKeyUpdateBody1),
			user:                      tnu1,
			skgID:                     skg1.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			verifyChildSpanner:        true,
			expectedName:              cutil.GetPtr("test-sshkeygroup-updated-1"),
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
			expectVersion:             skg1.Version,
			expectedSiteAssociations:  true,
			expectedSSHKeys:           true,
			countSSHKeys:              2,
			countSiteAssociations:     2,
		},
		{
			name:                      "success case when name, site and keys updated and existing site marked as deleting",
			reqOrgName:                tnOrg2,
			reqBody:                   string(okSiteKeyUpdateBody2),
			user:                      tnu2,
			skgID:                     skg2.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			verifyChildSpanner:        true,
			expectedName:              cutil.GetPtr("test-sshkeygroup-updated-2"),
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
			expectVersion:             skg2.Version,
			expectedSiteAssociations:  true,
			expectedSSHKeys:           true,
			countSSHKeys:              1,
			countSiteAssociations:     2,
			expectedDeletingSSHKeyGroupSiteAssociation: true,
			DeletingSSHKeyGroupSiteAssociationID:       skgsa2.ID,
		},
		{
			name:                      "success case when only deleting existing site and key associations",
			reqOrgName:                tnOrg2,
			reqBody:                   string(okDeleteAllSiteKeyBody1),
			user:                      tnu2,
			skgID:                     skg4.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			verifyChildSpanner:        true,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
			expectVersion:             skg4.Version,
			expectedSiteAssociations:  true,
			expectedSSHKeys:           true,
			countSSHKeys:              0,
			countSiteAssociations:     1,
			expectedDeletingSSHKeyGroupSiteAssociation: true,
			DeletingSSHKeyGroupSiteAssociationID:       skgsa4.ID,
		},
		{
			name:                      "success case when only Keys are updated for Key Group with no Site association",
			reqOrgName:                tnOrg,
			reqBody:                   string(okKeyUpdateBody7),
			user:                      tnu1,
			skgID:                     skg7.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
			expectedSiteAssociations:  false,
			expectedSSHKeys:           true,
			countSSHKeys:              2,
			countSiteAssociations:     0,
		},
		{
			name:                      "success case when only Sites are updated for Key Group with no Key association",
			reqOrgName:                tnOrg,
			reqBody:                   string(okSiteUpdateBody8),
			user:                      tnu1,
			skgID:                     skg8.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
			expectedSiteAssociations:  true,
			expectedSSHKeys:           false,
			countSSHKeys:              0,
			countSiteAssociations:     2,
		},
		{
			name:                      "success case with no changes to Site/Key Associations",
			reqOrgName:                tnOrg,
			reqBody:                   string(okSiteKeyUpdateBody9),
			user:                      tnu1,
			skgID:                     skg9.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
			expectedSiteAssociations:  true,
			expectedSSHKeys:           true,
			countSSHKeys:              1,
			countSiteAssociations:     1,
		},
		{
			name:                      "success case when Site/Key Associations are added to Key Group with no existing Association",
			reqOrgName:                tnOrg,
			reqBody:                   string(okSiteKeyUpdateBody10),
			user:                      tnu1,
			skgID:                     skg10.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
			expectedSiteAssociations:  true,
			expectedSSHKeys:           true,
			countSSHKeys:              1,
			countSiteAssociations:     1,
		},
		{
			name:                      "success case when the last Key Association is removed with no Site Association",
			reqOrgName:                tnOrg,
			reqBody:                   string(okSiteKeyUpdateBody11),
			user:                      tnu1,
			skgID:                     skg11.ID.String(),
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			expectedSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
			expectedSiteAssociations:  false,
			expectedSSHKeys:           false,
			countSSHKeys:              0,
			countSiteAssociations:     0,
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
			values := []string{tc.reqOrgName, tc.skgID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			uskgh := UpdateSSHKeyGroupHandler{
				dbSession: dbSession,
				tc:        tmc,
				cfg:       cfg,
			}
			err := uskgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISSHKeyGroup{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.skgID, rsp.ID)
				if tc.expectedName != nil {
					assert.Equal(t, *tc.expectedName, rsp.Name)
				}

				if tc.expectedSiteAssociations {
					assert.Equal(t, tc.countSiteAssociations, len(rsp.SiteAssociations))
				}

				if tc.expectedSSHKeys {
					assert.Equal(t, tc.countSSHKeys, len(rsp.SSHKeys))
				}

				if tc.expectVersion != nil {
					assert.NotEqual(t, *tc.expectVersion, *rsp.Version)
				}

				if tc.expectedSSHKeyGroupStatus != nil {
					assert.Equal(t, *tc.expectedSSHKeyGroupStatus, rsp.Status)
				}

				if tc.expectedDeletingSSHKeyGroupSiteAssociation {
					skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
					skgsa, _ := skgsaDAO.GetByID(ctx, nil, tc.DeletingSSHKeyGroupSiteAssociationID, nil)
					assert.Equal(t, skgsa.Status, cdbm.SSHKeyGroupSiteAssociationStatusDeleting)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupHandler_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Sites
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	st3 := testInstanceBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st3)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu2 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant2", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	sk1 := testBuildSSHKey(t, dbSession, "test1", tnOrg, tn1.ID, "testpublickey1", cutil.GetPtr("test1"), nil, tnu1.ID)
	assert.NotNil(t, sk1)

	sk2 := testBuildSSHKey(t, dbSession, "test2", tnOrg, tn1.ID, "testpublickey2", cutil.GetPtr("test2"), nil, tnu1.ID)
	assert.NotNil(t, sk2)

	sk3 := testBuildSSHKey(t, dbSession, "test3", tnOrg, tn1.ID, "testpublickey3", cutil.GetPtr("test3"), nil, tnu1.ID)
	assert.NotNil(t, sk3)

	skg1 := testBuildSSHKeyGroup(t, dbSession, "test", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSyncing, tnu1.ID)

	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1234"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa1)

	skgsa2 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st2.ID, cutil.GetPtr("5678"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa2)

	skgsa3 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st3.ID, cutil.GetPtr("9012"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa3)

	ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska1)

	ska2 := testBuildSSHKeyAssociation(t, dbSession, sk2.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska2)

	ska3 := testBuildSSHKeyAssociation(t, dbSession, sk3.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska3)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                     string
		reqOrgName               string
		user                     *cdbm.User
		skgID                    string
		expectedErr              bool
		expectedStatus           int
		expectedSiteAssociations bool
		countSiteAssociations    int
		expectedSSHKeys          bool
		countSSHKeys             int
		expectedTenantOrg        *string
		queryIncludeRelations1   *string
		verifyChildSpanner       bool
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
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when skg id is not uuid",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skgID:          "badid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when skg not found",
			reqOrgName:     tnOrg,
			user:           tnu1,
			skgID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when skg tenant doesnt match tenant in org",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			skgID:          skg1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                     "success case",
			reqOrgName:               tnOrg,
			user:                     tnu1,
			skgID:                    skg1.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedSiteAssociations: true,
			expectedSSHKeys:          true,
			countSiteAssociations:    3,
			countSSHKeys:             3,
			verifyChildSpanner:       true,
		},
		{
			name:                   "success case when include relation",
			reqOrgName:             tnOrg,
			user:                   tnu1,
			skgID:                  skg1.ID.String(),
			queryIncludeRelations1: cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			countSiteAssociations:  3,
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
			values := []string{tc.reqOrgName, tc.skgID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			dskgh := GetSSHKeyGroupHandler{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			}

			err := dskgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISSHKeyGroup{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.skgID, rsp.ID)
				if tc.queryIncludeRelations1 != nil {
					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
				} else {
					assert.Nil(t, rsp.Tenant)
				}
				assert.NotNil(t, rsp.SiteAssociations[0].Site.InfrastructureProvider)
				if tc.expectedSiteAssociations {
					assert.Equal(t, tc.countSiteAssociations, len(rsp.SiteAssociations))
				}

				if tc.expectedSSHKeys {
					assert.Equal(t, tc.countSSHKeys, len(rsp.SSHKeys))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)

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

	st3 := testInstanceBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st3)

	st4 := testInstanceBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st4)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)
	tnu2 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg2, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant1", tnOrg, tnu1)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant2", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	skgs := []*cdbm.SSHKeyGroup{}

	for i := 1; i <= 25; i++ {
		var status string
		if i%2 == 0 {
			status = cdbm.SSHKeyGroupStatusSynced
		} else {
			status = cdbm.SSHKeyGroupStatusSyncing
		}

		skg1 := testBuildSSHKeyGroup(t, dbSession, fmt.Sprintf("test-%d", i), tnOrg, nil, tn1.ID, nil, status, tnu1.ID)
		sk1 := testBuildSSHKey(t, dbSession, fmt.Sprintf("test-first-%d", i), tnOrg, tn1.ID, fmt.Sprintf("testpublickey1-%d", i), cutil.GetPtr("test"), nil, tnu1.ID)
		sk2 := testBuildSSHKey(t, dbSession, fmt.Sprintf("test-second-%d", i), tnOrg, tn1.ID, fmt.Sprintf("testpublickey2-%d", i), cutil.GetPtr("test"), nil, tnu1.ID)

		testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
		testBuildSSHKeyAssociation(t, dbSession, sk2.ID, skg1.ID, tnu1.ID)
		testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, nil, cdbm.SSHKeyGroupStatusSyncing, tnu1.ID)

		assert.NotNil(t, skg1)
		skgs = append(skgs, skg1)
	}

	// Build Instance
	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	skgia1 := testInstanceBuildSSHKeyGroupInstanceAssociation(t, dbSession, skgs[0].ID, st1.ID, inst1.ID)
	assert.NotNil(t, skgia1)

	skgia2 := testInstanceBuildSSHKeyGroupInstanceAssociation(t, dbSession, skgs[1].ID, st1.ID, inst1.ID)
	assert.NotNil(t, skgia2)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                     string
		reqOrgName               string
		reqSiteID                *uuid.UUID
		reqInstanceID            *uuid.UUID
		querySearch              *string
		queryIncludeRelations1   *string
		pageNumber               *int
		pageSize                 *int
		orderBy                  *string
		user                     *cdbm.User
		expectedErr              bool
		expectedStatus           int
		expectedSkgCnt           int
		expectedSiteAssociations bool
		countSiteAssociations    int
		expectedSSHKeys          bool
		countSSHKeys             int
		expectedTotal            *int
		expectedFirstSkName      *string
		expectedTenantOrg        *string
		queryStatus              *string
		verifyChildSpanner       bool
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
			name:                     "success case, objects returned",
			reqOrgName:               tnOrg,
			user:                     tnu1,
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedSkgCnt:           20,
			expectedTotal:            cutil.GetPtr(25),
			expectedSiteAssociations: true,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
			verifyChildSpanner:       true,
		},
		{
			name:                     "success case filtering by associated Site, objects returned",
			reqOrgName:               tnOrg,
			reqSiteID:                cutil.GetPtr(st1.ID),
			user:                     tnu1,
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedSkgCnt:           20,
			expectedTotal:            cutil.GetPtr(25),
			expectedSiteAssociations: true,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
			verifyChildSpanner:       true,
		},
		{
			name:                     "success case filtering by Site with no associations, no objects returned",
			reqOrgName:               tnOrg,
			reqSiteID:                cutil.GetPtr(st2.ID),
			user:                     tnu1,
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedSkgCnt:           0,
			expectedTotal:            cutil.GetPtr(0),
			expectedSiteAssociations: false,
			countSiteAssociations:    0,
			expectedSSHKeys:          false,
			countSSHKeys:             0,
			verifyChildSpanner:       true,
		},
		{
			name:                     "error case filtering by non-existent Site ID",
			reqOrgName:               tnOrg,
			reqSiteID:                cutil.GetPtr(uuid.New()),
			user:                     tnu1,
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedErr:              true,
			expectedStatus:           http.StatusBadRequest,
			expectedSkgCnt:           0,
			expectedTotal:            cutil.GetPtr(0),
			expectedSiteAssociations: false,
			countSiteAssociations:    0,
			expectedSSHKeys:          false,
			countSSHKeys:             0,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
			verifyChildSpanner:       true,
		},
		{
			name:                     "success case filtering by Instance, objects returned",
			reqOrgName:               tnOrg,
			reqInstanceID:            cutil.GetPtr(inst1.ID),
			user:                     tnu1,
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedSkgCnt:           2,
			expectedTotal:            cutil.GetPtr(2),
			expectedSiteAssociations: false,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
			verifyChildSpanner:       true,
		},
		{
			name:                     "error case filtering by non-existent Instance ID",
			reqOrgName:               tnOrg,
			reqInstanceID:            cutil.GetPtr(uuid.New()),
			user:                     tnu1,
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedErr:              true,
			expectedStatus:           http.StatusBadRequest,
			expectedSkgCnt:           0,
			expectedTotal:            cutil.GetPtr(0),
			expectedSiteAssociations: false,
			countSiteAssociations:    0,
			expectedSSHKeys:          false,
			countSSHKeys:             0,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
			verifyChildSpanner:       true,
		},
		{
			name:                     "success case filtering by both Site ID and Instance ID, objects returned",
			reqOrgName:               tnOrg,
			reqInstanceID:            cutil.GetPtr(inst1.ID),
			reqSiteID:                cutil.GetPtr(st1.ID),
			user:                     tnu1,
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedSkgCnt:           2,
			expectedTotal:            cutil.GetPtr(2),
			expectedSiteAssociations: false,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
			verifyChildSpanner:       true,
		},
		{
			name:                     "success case with paging",
			reqOrgName:               tnOrg,
			user:                     tnu1,
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			pageNumber:               cutil.GetPtr(1),
			pageSize:                 cutil.GetPtr(10),
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedSkgCnt:           10,
			expectedTotal:            cutil.GetPtr(25),
			expectedSiteAssociations: true,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
		},
		{
			name:                     "success case with paging set higher limit but returns expected higher count",
			reqOrgName:               tnOrg,
			user:                     tnu1,
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			pageNumber:               cutil.GetPtr(1),
			pageSize:                 cutil.GetPtr(30),
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedSkgCnt:           25,
			expectedTotal:            cutil.GetPtr(25),
			expectedSiteAssociations: true,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
			expectedFirstSkName:      cutil.GetPtr("test-1"),
		},
		{
			name:                     "success case retrieving second page",
			reqOrgName:               tnOrg,
			user:                     tnu1,
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			pageNumber:               cutil.GetPtr(2),
			pageSize:                 cutil.GetPtr(10),
			orderBy:                  cutil.GetPtr("CREATED_ASC"),
			expectedSkgCnt:           10,
			expectedTotal:            cutil.GetPtr(25),
			expectedFirstSkName:      cutil.GetPtr("test-11"),
			expectedSiteAssociations: true,
			countSiteAssociations:    1,
			expectedSSHKeys:          true,
			countSSHKeys:             2,
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
			expectedSkgCnt: 0,
		},
		{
			name:                   "success case with include relation",
			reqOrgName:             tnOrg,
			user:                   tnu1,
			orderBy:                cutil.GetPtr("CREATED_ASC"),
			queryIncludeRelations1: cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedSkgCnt:         20,
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
			expectedSkgCnt: 20,
			expectedTotal:  cutil.GetPtr(25),
		},
		{
			name:           "success case return empty list if no objects created",
			reqOrgName:     tnOrg2,
			user:           tnu2,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedSkgCnt: 0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success case filtering by status Syncing, objects returned",
			reqOrgName:     tnOrg,
			user:           tnu1,
			queryStatus:    cutil.GetPtr("Syncing"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedSkgCnt: 13,
			expectedTotal:  cutil.GetPtr(13),
		},
		{
			name:           "success case filtering by status Synced, objects returned",
			reqOrgName:     tnOrg,
			user:           tnu1,
			queryStatus:    cutil.GetPtr("Synced"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedSkgCnt: 12,
			expectedTotal:  cutil.GetPtr(12),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.reqSiteID != nil {
				q.Set("siteId", tc.reqSiteID.String())
			}
			if tc.reqInstanceID != nil {
				q.Set("instanceId", tc.reqInstanceID.String())
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
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.queryStatus != nil {
				q.Set("status", *tc.queryStatus)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/sshkeygroup?%s", tc.reqOrgName, q.Encode())

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

			gsgh := GetAllSSHKeyGroupHandler{
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

			rsp := []model.APISSHKeyGroup{}
			err = json.Unmarshal(rec.Body.Bytes(), &rsp)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedSkgCnt, len(rsp))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, pr.Total)
			}

			if tc.expectedSiteAssociations {
				if len(rsp) > 0 {
					assert.Equal(t, tc.countSiteAssociations, len((rsp)[0].SiteAssociations))
				}
			}

			if tc.expectedSSHKeys {
				if len(rsp) > 0 {
					assert.Equal(t, tc.countSSHKeys, len((rsp)[0].SSHKeys))
				}
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
			if len(rsp) > 0 {
				assert.NotNil(t, rsp[0].SiteAssociations[0].Site.InfrastructureProvider)
			}
			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tnOrgRolesForbidden := []string{"NICO_TENANT_USER"}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRoles)

	tnu1Forbidden := testInstanceBuildUser(t, dbSession, uuid.New().String(), tnOrg, tnOrgRolesForbidden)
	assert.NotNil(t, tnu1Forbidden)

	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	// Build SSHKeyGroup 1
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSynced, tnu1.ID)

	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, nil, cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa1)

	sk1 := testBuildSSHKey(t, dbSession, "test", tnOrg, tn1.ID, "testpublickey", cutil.GetPtr("test"), nil, tnu1.ID)
	ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tnu1.ID)
	assert.NotNil(t, ska1)

	// Build SSHKeyGroup 2
	skg2 := testBuildSSHKeyGroup(t, dbSession, "test-2", tnOrg, nil, tn1.ID, nil, cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	ska21 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg2.ID, tnu1.ID)
	assert.NotNil(t, ska21)

	cfg := common.GetTestConfig()

	tmc := &tmocks.Client{}
	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	// Mock delete call
	tmc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                    string
		reqOrgName              string
		user                    *cdbm.User
		skgID                   string
		expectErr               bool
		expectStatus            int
		expectSSHKeyGroupStatus *string
		expectDeletion          bool
		verifyChildSpanner      bool
	}{
		{
			name:         "error when user not found in request context",
			reqOrgName:   tnOrg,
			user:         nil,
			expectErr:    true,
			expectStatus: http.StatusInternalServerError,
		},
		{
			name:         "error when user not found in org",
			reqOrgName:   "SomeOrg",
			user:         tnu1,
			expectErr:    true,
			expectStatus: http.StatusForbidden,
		},
		{
			name:         "error when user role is forbidden",
			reqOrgName:   tnOrg,
			user:         tnu1Forbidden,
			expectErr:    true,
			expectStatus: http.StatusForbidden,
		},
		{
			name:         "error when tenant not in org",
			reqOrgName:   tnOrg2,
			user:         tnu1,
			expectErr:    true,
			expectStatus: http.StatusForbidden,
		},
		{
			name:         "error when skg id is not uuid",
			reqOrgName:   tnOrg,
			user:         tnu1,
			skgID:        "badid",
			expectErr:    true,
			expectStatus: http.StatusBadRequest,
		},
		{
			name:         "error when skg not found",
			reqOrgName:   tnOrg,
			user:         tnu1,
			skgID:        uuid.New().String(),
			expectErr:    true,
			expectStatus: http.StatusNotFound,
		},
		{
			name:                    "success case with Site associations",
			reqOrgName:              tnOrg,
			user:                    tnu1,
			skgID:                   skg1.ID.String(),
			expectErr:               false,
			expectStatus:            http.StatusAccepted,
			expectSSHKeyGroupStatus: cutil.GetPtr(cdbm.SSHKeyGroupStatusDeleting),
			verifyChildSpanner:      true,
		},
		{
			name:               "success case without Site association",
			reqOrgName:         tnOrg,
			user:               tnu1,
			skgID:              skg2.ID.String(),
			expectErr:          false,
			expectStatus:       http.StatusAccepted,
			expectDeletion:     true,
			verifyChildSpanner: true,
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
			values := []string{tc.reqOrgName, tc.skgID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			dskgh := DeleteSSHKeyGroupHandler{
				dbSession: dbSession,
				tc:        tmc,
				cfg:       cfg,
			}

			err := dskgh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectStatus, rec.Code)
			if !tc.expectErr {
				// verify sshkeygroup marked as  deleting
				skgDAO := cdbm.NewSSHKeyGroupDAO(dbSession)
				tid, err := uuid.Parse(tc.skgID)
				assert.Nil(t, err)

				skg, err := skgDAO.GetByID(ctx, nil, tid, nil)
				if tc.expectDeletion {
					assert.NotNil(t, err)
				} else {
					assert.Nil(t, err)

					if tc.expectSSHKeyGroupStatus != nil {
						assert.Equal(t, *tc.expectSSHKeyGroupStatus, skg.Status)
					}

					skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
					skgsasToSync, _, err := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{}, nil)
					assert.Nil(t, err)
					for _, sksa := range skgsasToSync {
						assert.Equal(t, sksa.Status, cdbm.SSHKeyGroupSiteAssociationStatusDeleting)
					}
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
