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
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
)

func testIPBlockInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// reset the tables needed for ipBlock tests
func testIPBlockSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create AllocationConstraints table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// setup ipam table
	ipamStorage := cipam.NewBunStorage(dbSession.DB, nil)
	assert.Nil(t, ipamStorage.ApplyDbSchema())
	assert.Nil(t, ipamStorage.DeleteAllPrefixes(context.Background(), ""))
}

func testIPBlockBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
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

func testIPBlockBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, cdbm.TenantCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Tenant"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return tn
}

func testIPBlockBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string) *cdbm.User {
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
			Email:       cutil.GetPtr("jdoe@test.com"),
			FirstName:   cutil.GetPtr("John"),
			LastName:    cutil.GetPtr("Doe"),
			OrgData:     OrgData,
		},
	)
	assert.Nil(t, err)

	return u
}

func testIPBlockBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion string, fullGrant bool, status string, user *cdbm.User) *cdbm.IPBlock {
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)
	ipb, err := ipbDAO.Create(
		context.Background(),
		nil,
		cdbm.IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenantID,
			RoutingType:              routingType,
			Prefix:                   prefix,
			PrefixLength:             blockSize,
			ProtocolVersion:          protocolVersion,
			FullGrant:                fullGrant,
			Status:                   status,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	return ipb
}

// testIPBlockBuildTenantSitePrefix creates a private Tenant SitePrefix. The
// combination of TenantID and SitePrefixID keeps it out of generic IPBlock
// handlers.
func testIPBlockBuildTenantSitePrefix(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, prefix string, prefixLength int, status string, user *cdbm.User) *cdbm.IPBlock {
	t.Helper()
	sitePrefixID := uuid.New()
	ipBlock, err := cdbm.NewIPBlockDAO(dbSession).Create(context.Background(), nil, cdbm.IPBlockCreateInput{
		Name:                     name,
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 &tenant.ID,
		SitePrefixID:             &sitePrefixID,
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:                   prefix,
		PrefixLength:             prefixLength,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
		Status:                   status,
		CreatedBy:                &user.ID,
	})
	require.NoError(t, err)
	return ipBlock
}

func testIPBlockBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID string, status string) {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID, Status: status, Message: nil})
	assert.Nil(t, err)
	assert.NotNil(t, ssd)
	assert.Equal(t, entityID, ssd.EntityID)
	assert.Equal(t, status, ssd.Status)
}

func testIPBlockBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, status string, imEnabled bool, user *cdbm.User) *cdbm.Site {
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
		IsInfinityEnabled:             imEnabled,
		SerialConsoleHostname:         cutil.GetPtr("TestSshHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Status:                        status,
		CreatedBy:                     user.ID,
	})
	assert.Nil(t, err)

	return st
}

func testIPBlockBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
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

func testIPBlockBuildAllocationConstraint(t *testing.T, dbSession *cdb.Session, allocationID uuid.UUID, resourceType string, resourceTypeID uuid.UUID, constraintType string, constraintValue int, derivedResourceID *uuid.UUID, createdBy uuid.UUID) *cdbm.AllocationConstraint {
	alcDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	alc, err := alcDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
		AllocationID: allocationID, ResourceType: resourceType, ResourceTypeID: resourceTypeID,
		ConstraintType: constraintType, ConstraintValue: constraintValue,
		DerivedResourceID: derivedResourceID, CreatedBy: createdBy,
	})
	assert.Nil(t, err)

	return alc
}

func TestIPBlockHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()

	testIPBlockSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"

	orgRoles := []string{authz.ProviderAdminRole}

	user := testIPBlockBuildUser(t, dbSession, "TestIPBlockHandler_Create", []string{ipOrg1, ipOrg2, ipOrg3, tnOrg1, tnOrg2}, orgRoles)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, user)
	assert.NotNil(t, ip2)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site2)

	prefLen24 := 24
	prefLen19 := 19
	prefLen15 := 15

	okBody1, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test1",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	okBodyUsePrefixLength, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test-with-prefix-len",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.169.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test2",
		SiteID:          site2.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	errBodyNameClash, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test1",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.1.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	okBody3, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test-3",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.1.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	errDoesntValidate, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test4",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "badprefix",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)

	errBadSiteID, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test5",
		SiteID:          uuid.New().String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)

	errIPPrefixClash, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "test6",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	publicSamePrefix, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "public-same-prefix",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypePublic,
		Prefix:          "192.168.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	lockBusyBody, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "site-fabric-lock-busy",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.172.0.0",
		PrefixLength:    prefLen24,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	errBody1, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "errortest",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "192.168.0.0",
		PrefixLength:    prefLen15,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)
	errBody2, err := json.Marshal(&model.APIIPBlockCreateRequest{
		Name:            "errortest",
		SiteID:          site.ID.String(),
		RoutingType:     cdbm.IPBlockRoutingTypeDatacenterOnly,
		Prefix:          "100.100.11.11",
		PrefixLength:    prefLen19,
		ProtocolVersion: cdbm.IPBlockProtocolVersionV4})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		reqBody            string
		user               *cdbm.User
		paramNamespace     string
		paramCIDR          string
		expectedErr        bool
		expectedStatus     int
		expectedIpam       bool
		expectedIpamErrMsg string
		expectedErrorText  string
		expectMessage      *string
		verifyChildSpanner bool
		holdSiteFabricLock bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody1),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body doesnt bind",
			reqOrgName:     ipOrg1,
			reqBody:        "SomeNonJsonBody",
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when request doesnt validate",
			reqOrgName:     ipOrg1,
			reqBody:        string(errDoesntValidate),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site in request doesnt exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBadSiteID),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when infrastructureProvider doesnt exist for org",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBody1),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when org has no infrastructure provider",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBody1),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when infrastructure in requestSite does not match that in org",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody2),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case in one infrastructure provider",
			reqOrgName:         ipOrg1,
			reqBody:            string(okBody1),
			user:               user,
			expectedErr:        false,
			expectedStatus:     http.StatusCreated,
			paramNamespace:     ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ip.ID.String(), site.ID.String()),
			paramCIDR:          ipam.GetCidrForIPBlock(ctx, "192.168.0.1", 24),
			expectedIpam:       true,
			expectMessage:      cutil.GetPtr("IP Block is ready for use"),
			verifyChildSpanner: true,
		},
		{
			name:           "success case when prefix length is used",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyUsePrefixLength),
			user:           user,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			paramNamespace: ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ip.ID.String(), site.ID.String()),
			paramCIDR:      ipam.GetCidrForIPBlock(ctx, "192.169.0.1", 24),
			expectedIpam:   true,
			expectMessage:  cutil.GetPtr("IP Block is ready for use"),
		},
		{
			name:              "error when ip prefix clashes in same infrastructure provider",
			reqOrgName:        ipOrg1,
			reqBody:           string(errIPPrefixClash),
			user:              user,
			expectedErr:       true,
			expectedStatus:    http.StatusConflict,
			expectedErrorText: "IPBlock with prefix: 192.168.0.0 and prefix_length: 24",
		},
		{
			name:           "success when the same prefix uses another routing type",
			reqOrgName:     ipOrg1,
			reqBody:        string(publicSamePrefix),
			user:           user,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			paramNamespace: ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypePublic, ip.ID.String(), site.ID.String()),
			paramCIDR:      ipam.GetCidrForIPBlock(ctx, "192.168.0.0", 24),
			expectedIpam:   true,
		},
		{
			name:               "conflict while Site fabric IP Blocks are being updated",
			reqOrgName:         ipOrg1,
			reqBody:            string(lockBusyBody),
			user:               user,
			expectedErr:        true,
			expectedStatus:     http.StatusConflict,
			expectedErrorText:  "Site fabric IP Blocks are being updated; retry the request",
			holdSiteFabricLock: true,
		},
		{
			name:           "error when ip prefix does not match block size",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody2),
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectMessage:  cutil.GetPtr("prefix should have 13 right most bits zeroed to match block size, e.g. 100.100.0.0"),
		},
		{
			name:           "success when ip prefix clashes for another infrastructure provider",
			reqOrgName:     ipOrg2,
			reqBody:        string(okBody2),
			user:           user,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			paramNamespace: ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ip2.ID.String(), site2.ID.String()),
			paramCIDR:      ipam.GetCidrForIPBlock(ctx, "192.168.0.0", 24),
			expectedIpam:   true,
		},
		{
			name:               "error due to name clash in same infrastructure provider",
			reqOrgName:         ipOrg1,
			reqBody:            string(errBodyNameClash),
			user:               user,
			expectedErr:        true,
			expectedStatus:     http.StatusConflict,
			paramNamespace:     ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ip.ID.String(), site.ID.String()),
			paramCIDR:          ipam.GetCidrForIPBlock(ctx, "192.168.1.0", 24),
			expectedIpamErrMsg: "Failed",
		},
		{
			name:           "success with another prefix in same infrastructure provider",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody3),
			user:           user,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			paramNamespace: ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ip.ID.String(), site.ID.String()),
			paramCIDR:      ipam.GetCidrForIPBlock(ctx, "192.168.1.0", 24),
			expectedIpam:   true,
		},
		{
			name:               "error creating ipam entry",
			reqOrgName:         ipOrg1,
			reqBody:            string(errBody1),
			user:               user,
			expectedErr:        true,
			expectedStatus:     http.StatusConflict,
			paramNamespace:     ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ip.ID.String(), site.ID.String()),
			paramCIDR:          ipam.GetCidrForIPBlock(ctx, "192.168.0.0", 24),
			expectedIpam:       true,
			expectedIpamErrMsg: "Could not create IPAM entry for IPBlock. Details: 192.168.0.0/15 overlaps 192.168.0.0/24",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			if tc.holdSiteFabricLock {
				lockingTx, err := cdb.BeginTx(ctx, dbSession, nil)
				require.NoError(t, err)
				t.Cleanup(func() {
					assert.NoError(t, lockingTx.Rollback())
				})
				require.NoError(t, lockingTx.AcquireAdvisoryLock(
					ctx,
					cdbm.SiteFabricIPBlockLockID(site.InfrastructureProviderID, site.ID),
					false,
				))
			}
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

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			cipbh := CreateIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := cipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIIPBlock{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate the status
				assert.Equal(t, rsp.Status, cdbm.IPBlockStatusReady)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 1)
				// validate message fields
				if tc.expectMessage != nil {
					assert.Equal(t, rsp.StatusHistory[0].Message, tc.expectMessage)
				}
				// validate ipam exists
				if tc.expectedIpam {
					ipamer := cipam.NewWithStorage(ipamStorage)
					ipamer.SetNamespace(tc.paramNamespace)
					pref := ipamer.PrefixFrom(ctx, tc.paramCIDR)
					assert.NotNil(t, pref)
					assert.Equal(t, pref.Namespace, tc.paramNamespace)
				}
			} else {
				fmt.Printf("error message body : %s", string(rec.Body.Bytes()))
				if tc.expectedErrorText != "" {
					assert.Contains(t, rec.Body.String(), tc.expectedErrorText)
				}
				if tc.expectedIpam && tc.expectedIpamErrMsg != "" {
					assert.Contains(t, rec.Body.String(), tc.expectedIpamErrMsg)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestIPBlockHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()

	testIPBlockSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testIPBlockBuildUser(t, dbSession, "TestIPBlockHandler_Update", []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, user)
	assert.NotNil(t, ip2)

	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site2)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testDel1", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb1)
	testIPBlockBuildStatusDetail(t, dbSession, ipb1.ID.String(), cdbm.IPBlockStatusPending)
	ipb3 := testIPBlockBuildIPBlock(t, dbSession, "testDel2", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.2.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb3)

	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "testDel2", site2, ip2, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb2)
	testIPBlockBuildStatusDetail(t, dbSession, ipb2.ID.String(), cdbm.IPBlockStatusPending)
	tenant := testIPBlockBuildTenant(t, dbSession, "private-tenant", "private-tenant", user)
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site, ip, tenant, "10.202.0.0", 24, cdbm.IPBlockStatusReady, user)

	errBody1, err := json.Marshal(model.APIIPBlockUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(model.APIIPBlockUpdateRequest{Name: cutil.GetPtr("testDel2")})
	assert.Nil(t, err)
	okBody1, err := json.Marshal(model.APIIPBlockUpdateRequest{Name: cutil.GetPtr("UpdatedName1")})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(model.APIIPBlockUpdateRequest{Name: cutil.GetPtr("UpdatedName2"), Description: cutil.GetPtr("UpdatedDesc2")})
	assert.Nil(t, err)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		reqBody            string
		reqIpb             *cdbm.IPBlock
		user               *cdbm.User
		ipbID              string
		expectedErr        bool
		expectedStatus     int
		expectedName       string
		expectedDesc       *string
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			reqIpb:         ipb1,
			user:           nil,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody1),
			reqIpb:         ipb1,
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when reqBody doesnt bind",
			reqOrgName:     ipOrg1,
			reqBody:        "BadBody",
			reqIpb:         ipb1,
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when reqBody json doesnt validate",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody1),
			reqIpb:         ipb1,
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have infrastructure provider",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBody1),
			user:           user,
			reqIpb:         ipb1,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when specified org does not have infrastructure provider matching the one in ipblock",
			reqOrgName:     ipOrg2,
			reqBody:        string(okBody1),
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when specified ipblock id is invalid uuid",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			reqIpb:         ipb1,
			user:           user,
			ipbID:          "bad$uuid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified ipblock doesnt exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			reqIpb:         ipb1,
			user:           user,
			ipbID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error due to name clash",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyNameClash),
			reqIpb:         ipb1,
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "provider cannot update a Tenant SitePrefix",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			reqIpb:         tenantSitePrefix,
			user:           user,
			ipbID:          tenantSitePrefix.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:               "success case 1",
			reqOrgName:         ipOrg1,
			reqBody:            string(okBody1),
			reqIpb:             ipb1,
			user:               user,
			ipbID:              ipb1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedName:       "UpdatedName1",
			expectedDesc:       nil,
			verifyChildSpanner: true,
		},
		{
			name:           "success case 2",
			reqOrgName:     ipOrg2,
			reqBody:        string(okBody2),
			reqIpb:         ipb2,
			user:           user,
			ipbID:          ipb2.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "UpdatedName2",
			expectedDesc:   cutil.GetPtr("UpdatedDesc2"),
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
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.ipbID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := UpdateIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIIPBlock{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, 1, len(rsp.StatusHistory))
				assert.Equal(t, tc.expectedName, rsp.Name)
				if tc.expectedDesc != nil {
					assert.Equal(t, *tc.expectedDesc, *rsp.Description)
				}
				assert.NotEqual(t, tc.reqIpb.Updated.String(), rsp.Updated.String())
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
	// A scoped 404 must leave the private record untouched. Read it without the
	// handler filter to distinguish hiding it from accidentally updating it.
	tenantSitePrefixAfter, err := cdbm.NewIPBlockDAO(dbSession).GetByID(ctx, nil, tenantSitePrefix.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, "private-site-prefix", tenantSitePrefixAfter.Name)
}

func TestIPBlockHandler_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()

	testIPBlockSetupSchema(t, dbSession)

	// ipam storage
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipOrg5 := "test-ip-org-5"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg5}, ipRoles)
	ipuv := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg5}, ipvRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	ip3 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp3", ipOrg3, ipu)
	assert.NotNil(t, ip3)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)
	site3 := testIPBlockBuildSite(t, dbSession, ip3, "testSite3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site3)

	tn := testIPBlockBuildTenant(t, dbSession, "testTenant", tnOrg1, tnu)
	assert.NotNil(t, tn)
	tn2 := testIPBlockBuildTenant(t, dbSession, "testTenant2", tnOrg2, tnu)
	assert.NotNil(t, tn2)
	tn3 := testIPBlockBuildTenant(t, dbSession, "testTenant3", ipOrg1, tnu)
	assert.NotNil(t, tn2)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "test1", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipb1)

	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	testIPBlockBuildStatusDetail(t, dbSession, ipb1.ID.String(), cdbm.IPBlockStatusPending)
	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "test2", site2, ip2, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, true, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipb2)

	parentPref2, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref2)

	ipb3 := testIPBlockBuildIPBlock(t, dbSession, "test3", site3, ip3, cutil.GetPtr(tn.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipb3)
	testIPBlockBuildStatusDetail(t, dbSession, ipb3.ID.String(), cdbm.IPBlockStatusPending)

	childPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb3.Prefix, ipb3.PrefixLength, ipb3.RoutingType, ipb3.InfrastructureProviderID.String(), ipb3.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, childPref1)

	ipb4 := testIPBlockBuildIPBlock(t, dbSession, "test3", site3, ip3, cutil.GetPtr(tn2.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipb4)

	ipb5 := testIPBlockBuildIPBlock(t, dbSession, "test5", site, ip, cutil.GetPtr(tn3.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipb5)
	testIPBlockBuildStatusDetail(t, dbSession, ipb5.ID.String(), cdbm.IPBlockStatusPending)
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site, ip, tn, "10.201.0.0", 24, cdbm.IPBlockStatusPending, tnu)

	alloc := testIPBlockBuildAllocation(t, dbSession, site3, tn, "testAlloc", ipu)
	allocConstraint := testIPBlockBuildAllocationConstraint(t, dbSession, alloc.ID, cdbm.AllocationResourceTypeIPBlock, ipb3.ID, cdbm.AllocationConstraintTypeOnDemand, 10, nil, ipu.ID)
	assert.NotNil(t, allocConstraint)

	// Generate data for service account org
	sOrg := "test-service-org"
	sRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	su := testSiteBuildUser(t, dbSession, uuid.NewString(), sOrg, sRoles)
	sip := testSiteBuildInfrastructureProvider(t, dbSession, "Test Service Provider", sOrg, su)
	stn := testSiteBuildTenant(t, dbSession, "Test Service Tenant", sOrg, su)

	ss := testSiteBuildSite(t, dbSession, sip, "test-service-site", cdbm.SiteStatusRegistered, su, nil, nil, nil)
	common.TestBuildTenantSite(t, dbSession, stn, ss, su)

	sipb := testIPBlockBuildIPBlock(t, dbSession, "site-prefix-1", ss, sip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, true, cdbm.IPBlockStatusPending, su)
	assert.NotNil(t, sipb)
	common.TestBuildStatusDetail(t, dbSession, sipb.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

	sipbprefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, sipb.Prefix, sipb.PrefixLength, sipb.RoutingType, sipb.InfrastructureProviderID.String(), sipb.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, sipbprefix)

	sa := testIPBlockBuildAllocation(t, dbSession, site, tn, fmt.Sprintf("tenant-ip-allocation-%s", sipb.Name), ipu)
	sac := testIPBlockBuildAllocationConstraint(t, dbSession, sa.ID, cdbm.AllocationResourceTypeIPBlock, sipb.ID, cdbm.AllocationConstraintTypeReserved, 24, nil, ipu.ID)
	assert.NotNil(t, sac)

	dsipb := testIPBlockBuildIPBlock(t, dbSession, "tenant-prefix-1", ss, sip, &stn.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.168.2.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, su)
	assert.NotNil(t, dsipb)
	common.TestBuildStatusDetail(t, dbSession, dsipb.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

	dsipbprefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, dsipb.Prefix, dsipb.PrefixLength, dsipb.RoutingType, dsipb.InfrastructureProviderID.String(), dsipb.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, dsipbprefix)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		ipbID                             string
		queryIncludeProviderRelation      *string
		queryIncludeTenantRelation        *string
		queryIncludeSiteRelation          *string
		queryIncludeUsageStats            *string
		expectedErr                       bool
		expectedIpam                      bool
		expectPerfUsage                   *cipam.Usage
		expectedStatus                    int
		expectedID                        string
		expectedStatusDetailsCnt          int
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		expectedTenantOrg                 *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when IP Block id is not a valid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			ipbID:          "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when IP Block does not exist for ID",
			reqOrgName:     tnOrg1,
			user:           tnu,
			ipbID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when Infrastructure Provider was not initialized for org",
			reqOrgName:     ipOrg5,
			user:           ipu,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when IP Block is not associated with Provider",
			reqOrgName:     ipOrg1,
			user:           ipu,
			ipbID:          ipb2.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when IP Block is not associated with Tenant",
			reqOrgName:     tnOrg1,
			user:           tnu,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "provider cannot retrieve a Tenant SitePrefix",
			reqOrgName:     ipOrg1,
			user:           ipu,
			ipbID:          tenantSitePrefix.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "tenant cannot retrieve its SitePrefix through the generic IPBlock API",
			reqOrgName:     tnOrg1,
			user:           tnu,
			ipbID:          tenantSitePrefix.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:                     "success when retrieving IP Block as Provider with admin role",
			reqOrgName:               ipOrg1,
			user:                     ipu,
			ipbID:                    ipb1.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               ipb1.ID.String(),
			expectedStatusDetailsCnt: 1,
			verifyChildSpanner:       true,
		},
		{
			name:                     "success when retrieving IP Block as Provider with viewer role",
			reqOrgName:               ipOrg1,
			user:                     ipuv,
			ipbID:                    ipb1.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               ipb1.ID.String(),
			expectedStatusDetailsCnt: 1,
			verifyChildSpanner:       true,
		},
		{
			name:                     "success when retrieving IP Block as Tenant",
			reqOrgName:               tnOrg1,
			user:                     tnu,
			ipbID:                    ipb3.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               ipb3.ID.String(),
			expectedStatusDetailsCnt: 1,
		},
		{
			name:                     "success when retrieving Provider IP Block as service account",
			reqOrgName:               sOrg,
			user:                     su,
			ipbID:                    sipb.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               sipb.ID.String(),
			expectedStatusDetailsCnt: 1,
		},
		{
			name:                     "success when retrieving Tenant IP Block as service account",
			reqOrgName:               sOrg,
			user:                     su,
			ipbID:                    dsipb.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               dsipb.ID.String(),
			expectedStatusDetailsCnt: 1,
		},
		{
			name:                              "success when retrieving IP Block with include relation",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			ipbID:                             ipb5.ID.String(),
			queryIncludeProviderRelation:      cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeTenantRelation:        cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeSiteRelation:          cutil.GetPtr(cdbm.SiteRelationName),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        ipb5.ID.String(),
			expectedStatusDetailsCnt:          1,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(site.Name),
			expectedTenantOrg:                 cutil.GetPtr(tn3.Org),
		},
		{
			name:                     "success when retrieving IP Block with ipam usage stats as Provider",
			reqOrgName:               ipOrg1,
			user:                     ipu,
			ipbID:                    ipb1.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               ipb1.ID.String(),
			expectedStatusDetailsCnt: 1,
			queryIncludeUsageStats:   cutil.GetPtr("true"),
			expectedIpam:             true,
			expectPerfUsage: &cipam.Usage{
				AvailableIPs:              parentPref1.Usage().AvailableIPs,
				AcquiredIPs:               parentPref1.Usage().AcquiredIPs,
				AcquiredPrefixes:          parentPref1.Usage().AcquiredPrefixes,
				AvailableSmallestPrefixes: parentPref1.Usage().AvailableSmallestPrefixes,
				AvailablePrefixes:         parentPref1.Usage().AvailablePrefixes,
			},
		},
		{
			name:                     "success when retrieving full grant IP Block with ipam usage stats as Provider",
			reqOrgName:               ipOrg2,
			user:                     ipu,
			ipbID:                    ipb2.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               ipb2.ID.String(),
			expectedStatusDetailsCnt: 0,
			queryIncludeUsageStats:   cutil.GetPtr("true"),
			expectedIpam:             true,
			expectPerfUsage: &cipam.Usage{
				AvailableIPs:              0,
				AcquiredIPs:               0,
				AcquiredPrefixes:          1,
				AvailableSmallestPrefixes: 0,
				AvailablePrefixes:         nil,
			},
		},
		{
			name:                     "success when retrieving IP Block with ipam usage stats as Tenant",
			reqOrgName:               tnOrg1,
			user:                     tnu,
			ipbID:                    ipb3.ID.String(),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedID:               ipb3.ID.String(),
			expectedStatusDetailsCnt: 1,
			queryIncludeUsageStats:   cutil.GetPtr("true"),
			expectedIpam:             true,
			expectPerfUsage: &cipam.Usage{
				AvailableIPs:              childPref1.Usage().AvailableIPs,
				AcquiredIPs:               childPref1.Usage().AcquiredIPs,
				AcquiredPrefixes:          childPref1.Usage().AcquiredPrefixes,
				AvailableSmallestPrefixes: childPref1.Usage().AvailableSmallestPrefixes,
				AvailablePrefixes:         childPref1.Usage().AvailablePrefixes,
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)
			q := req.URL.Query()
			if tc.queryIncludeProviderRelation != nil {
				q.Add("includeRelation", *tc.queryIncludeProviderRelation)
			}
			if tc.queryIncludeTenantRelation != nil {
				q.Add("includeRelation", *tc.queryIncludeTenantRelation)
			}
			if tc.queryIncludeSiteRelation != nil {
				q.Add("includeRelation", *tc.queryIncludeSiteRelation)
			}
			if tc.queryIncludeUsageStats != nil {
				q.Add("includeUsageStats", *tc.queryIncludeUsageStats)
			}
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.URL.RawQuery = q.Encode()
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.ipbID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := GetIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIIPBlock{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedID, rsp.ID)
				assert.Equal(t, tc.expectedStatusDetailsCnt, len(rsp.StatusHistory))
				if tc.queryIncludeProviderRelation != nil || tc.queryIncludeTenantRelation != nil || tc.queryIncludeSiteRelation != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProvider.Org)
					}
					if tc.expectedSiteName != nil {
						assert.Equal(t, *tc.expectedSiteName, rsp.Site.Name)
					}
					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
				} else {
					assert.Nil(t, rsp.InfrastructureProvider)
					assert.Nil(t, rsp.Site)
					assert.Nil(t, rsp.Tenant)
				}
				if tc.expectedIpam {
					assert.NotNil(t, rsp.UsageStats)
					assert.Equal(t, int(rsp.UsageStats.AcquiredIPs), int(tc.expectPerfUsage.AcquiredIPs))
					assert.Equal(t, int(rsp.UsageStats.AcquiredPrefixes), int(tc.expectPerfUsage.AcquiredPrefixes))
					assert.Equal(t, int(rsp.UsageStats.AvailableIPs), int(tc.expectPerfUsage.AvailableIPs))
					assert.Equal(t, len(rsp.UsageStats.AvailablePrefixes), len(tc.expectPerfUsage.AvailablePrefixes))
					assert.Equal(t, int(rsp.UsageStats.AvailableSmallestPrefixes), int(tc.expectPerfUsage.AvailableSmallestPrefixes))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestIPBlockHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()

	testIPBlockSetupSchema(t, dbSession)

	// ipam storage
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Add user entry
	ipOrg1 := "test-provider-org-1"
	ipOrg2 := "test-provider-org-2"
	ipOrg3 := "test-provider-org-3"
	ipOrg4 := "test-provider-org-4"
	ipOrg5 := "test-provider-org-5"

	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"

	tnRoles := []string{authz.TenantAdminRole}

	ipu := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg4, ipOrg5}, ipRoles)
	ipuv := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg4, ipOrg5}, ipvRoles)
	tnu := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "test-ip-block", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "test-ip-block-2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	ip3 := testIPBlockBuildInfrastructureProvider(t, dbSession, "test-ip-block-3", ipOrg3, ipu)
	assert.NotNil(t, ip3)
	ip4 := testIPBlockBuildInfrastructureProvider(t, dbSession, "test-ip-block-4", ipOrg4, ipu)
	assert.NotNil(t, ip4)

	site := testIPBlockBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)
	site3 := testIPBlockBuildSite(t, dbSession, ip3, "test-site-3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site3)
	site4 := testIPBlockBuildSite(t, dbSession, ip4, "test-site-4", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site4)

	tn := testIPBlockBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu)
	assert.NotNil(t, tn)
	tn2 := testIPBlockBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu)
	assert.NotNil(t, tn2)
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-tenant-site-prefix", site, ip, tn, "10.200.0.0", 24, cdbm.IPBlockStatusPending, tnu)

	totalCount := 30

	// Create IPAM entry IPBlock
	ipbfg := testIPBlockBuildIPBlock(t, dbSession, "test-ipam-1", site4, ip4, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, true, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipbfg)
	testIPBlockBuildStatusDetail(t, dbSession, ipbfg.ID.String(), cdbm.IPBlockStatusPending)
	common.TestBuildStatusDetail(t, dbSession, ipbfg.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

	ipbfgprefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbfg.Prefix, ipbfg.PrefixLength, ipbfg.RoutingType, ipbfg.InfrastructureProviderID.String(), ipbfg.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, ipbfgprefix)

	ipbwfg := testIPBlockBuildIPBlock(t, dbSession, "test-ipam-2", site4, ip4, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.169.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, ipbwfg)
	testIPBlockBuildStatusDetail(t, dbSession, ipbwfg.ID.String(), cdbm.IPBlockStatusPending)
	common.TestBuildStatusDetail(t, dbSession, ipbwfg.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

	ipbwfgprefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbwfg.Prefix, ipbwfg.PrefixLength, ipbwfg.RoutingType, ipbwfg.InfrastructureProviderID.String(), ipbwfg.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, ipbwfgprefix)

	ipbs := []cdbm.IPBlock{}

	for i := 0; i < totalCount; i++ {
		var ipb *cdbm.IPBlock

		if i%2 == 0 {
			ipb = testIPBlockBuildIPBlock(t, dbSession, fmt.Sprintf("test-%02d", i), site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, fmt.Sprintf("192.168.%v.0", i), 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
			assert.NotNil(t, ipb)
			testIPBlockBuildStatusDetail(t, dbSession, ipb.ID.String(), cdbm.IPBlockStatusPending)
		} else {
			ipb = testIPBlockBuildIPBlock(t, dbSession, fmt.Sprintf("test-%02d", i), site, ip, cutil.GetPtr(tn.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, fmt.Sprintf("192.168.%v.0", i), 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
			assert.NotNil(t, ipb)
			testIPBlockBuildStatusDetail(t, dbSession, ipb.ID.String(), cdbm.IPBlockStatusPending)

			a := testIPBlockBuildAllocation(t, dbSession, site, tn, fmt.Sprintf("test-ipblock-alloc-%02d", i), ipu)
			ac := testIPBlockBuildAllocationConstraint(t, dbSession, a.ID, cdbm.AllocationResourceTypeIPBlock, ipb.ID, cdbm.AllocationConstraintTypeReserved, 28, nil, ipu.ID)
			assert.NotNil(t, ac)
		}

		common.TestBuildStatusDetail(t, dbSession, ipb.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

		ipbs = append(ipbs, *ipb)
	}

	// Generate data for service account org
	sOrg := "test-service-org"
	sRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	su := testSiteBuildUser(t, dbSession, uuid.NewString(), sOrg, sRoles)
	sip := testSiteBuildInfrastructureProvider(t, dbSession, "Test Service Provider", sOrg, su)
	stn := testSiteBuildTenant(t, dbSession, "Test Service Tenant", sOrg, su)

	ss := testSiteBuildSite(t, dbSession, sip, "test-service-site", cdbm.SiteStatusRegistered, su, nil, nil, nil)
	common.TestBuildTenantSite(t, dbSession, stn, ss, su)

	sipb := testIPBlockBuildIPBlock(t, dbSession, "site-prefix-1", ss, sip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, true, cdbm.IPBlockStatusPending, su)
	assert.NotNil(t, sipb)
	testIPBlockBuildStatusDetail(t, dbSession, sipb.ID.String(), cdbm.IPBlockStatusPending)
	common.TestBuildStatusDetail(t, dbSession, sipb.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

	sipbprefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, sipb.Prefix, sipb.PrefixLength, sipb.RoutingType, sipb.InfrastructureProviderID.String(), sipb.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, sipbprefix)

	sa := testIPBlockBuildAllocation(t, dbSession, site, tn, fmt.Sprintf("tenant-ip-allocation-%s", sipb.Name), ipu)
	sac := testIPBlockBuildAllocationConstraint(t, dbSession, sa.ID, cdbm.AllocationResourceTypeIPBlock, sipb.ID, cdbm.AllocationConstraintTypeReserved, 24, nil, ipu.ID)
	assert.NotNil(t, sac)

	dsipb := testIPBlockBuildIPBlock(t, dbSession, "tenant-prefix-1", ss, sip, &stn.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.168.2.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, su)
	assert.NotNil(t, dsipb)
	testIPBlockBuildStatusDetail(t, dbSession, dsipb.ID.String(), cdbm.IPBlockStatusPending)
	common.TestBuildStatusDetail(t, dbSession, dsipb.ID.String(), cdbm.IPBlockStatusReady, cutil.GetPtr("IP Block is ready for use"))

	dsipbprefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, dsipb.Prefix, dsipb.PrefixLength, dsipb.RoutingType, dsipb.InfrastructureProviderID.String(), dsipb.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, dsipbprefix)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		querySiteID                       *string
		querySearch                       *string
		queryStatus                       *string
		queryIncludeProviderRelation      *string
		queryIncludeTenantRelation        *string
		queryIncludeSiteRelation          *string
		queryIncludeUsageStats            *string
		expectedIpam                      bool
		expectPerfUsage                   *cipam.Usage
		expectFGPerfUsage                 *cipam.Usage
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.IPBlock
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		expectedTenantOrg                 *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when site id not valid uuid",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr("non-uuid"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Site ID is non-existent",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(uuid.NewString()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "success when retreiving as Provider with admin role",
			reqOrgName:     ipOrg1,
			user:           ipu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when retreiving as Provider with viewer role",
			reqOrgName:     ipOrg1,
			user:           ipuv,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when retreiving as Tenant",
			reqOrgName:     tnOrg1,
			user:           tnu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when retrieving as service account",
			reqOrgName:     sOrg,
			user:           su,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    2,
		},
		{
			name:           "success when filtering by Site ID as Tenant",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when filtering by Site with no IP Blocks",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySiteID:    cutil.GetPtr(site2.ID.String()),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
		},
		{
			name:           "success when no IP Blocks exist for Provider",
			reqOrgName:     ipOrg2,
			user:           ipu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
		},
		{
			name:               "success when pagination params are specified",
			reqOrgName:         ipOrg1,
			user:               ipu,
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(10),
			orderBy:            cutil.GetPtr("NAME_DESC"),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        10,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectedFirstEntry: &ipbs[28],
		},
		{
			name:           "failure when invalid pagination params are specified",
			reqOrgName:     ipOrg1,
			user:           ipu,
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:                              "success when include relation is specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			queryIncludeProviderRelation:      cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeTenantRelation:        cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeSiteRelation:          cutil.GetPtr(cdbm.SiteRelationName),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       totalCount / 2,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(site.Name),
			expectedTenantOrg:                 cutil.GetPtr(tn.Org),
		},
		{
			name:           "success when name query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("test"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "Tenant SitePrefix is absent from search and pagination total",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr(tenantSitePrefix.Name),
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success when status query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("pending"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when search query containing name and status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("test ready"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when search query containing status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryStatus:    cutil.GetPtr(cdbm.IPBlockStatusPending),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
		},
		{
			name:           "success when search query containing invalid status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryStatus:    cutil.GetPtr("BadStatus"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:                   "success when retrieving as Provider with full grant IP Blocks",
			reqOrgName:             ipOrg4,
			user:                   ipu,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            2,
			expectedIpam:           true,
			queryIncludeUsageStats: cutil.GetPtr("true"),
			expectFGPerfUsage: &cipam.Usage{
				AvailableIPs:              0,
				AcquiredIPs:               0,
				AcquiredPrefixes:          1,
				AvailableSmallestPrefixes: 0,
				AvailablePrefixes:         nil,
			},
			expectPerfUsage: &cipam.Usage{
				AvailableIPs:              ipbwfgprefix.Usage().AvailableIPs,
				AcquiredIPs:               ipbwfgprefix.Usage().AcquiredIPs,
				AcquiredPrefixes:          ipbwfgprefix.Usage().AcquiredPrefixes,
				AvailableSmallestPrefixes: ipbwfgprefix.Usage().AvailableSmallestPrefixes,
				AvailablePrefixes:         ipbwfgprefix.Usage().AvailablePrefixes,
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.querySiteID != nil {
				q.Add("siteId", *tc.querySiteID)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.queryStatus != nil {
				q.Add("status", *tc.queryStatus)
			}
			if tc.queryIncludeProviderRelation != nil {
				q.Add("includeRelation", *tc.queryIncludeProviderRelation)
			}
			if tc.queryIncludeTenantRelation != nil {
				q.Add("includeRelation", *tc.queryIncludeTenantRelation)
			}
			if tc.queryIncludeSiteRelation != nil {
				q.Add("includeRelation", *tc.queryIncludeSiteRelation)
			}
			if tc.queryIncludeUsageStats != nil {
				q.Add("includeUsageStats", *tc.queryIncludeUsageStats)
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

			path := fmt.Sprintf("/v2/org/%s/nico/ipblock?%s", tc.reqOrgName, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			gaipbh := GetAllIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}

			err := gaipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			if tc.expectedErr {
				return
			}

			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)

			resp := []model.APIIPBlock{}
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
				assert.Equal(t, tc.expectedFirstEntry.Name, resp[0].Name)
			}

			if tc.queryIncludeProviderRelation != nil || tc.queryIncludeTenantRelation != nil || tc.queryIncludeSiteRelation != nil {
				if tc.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProvider.Org)
				}
				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
				if tc.expectedTenantOrg != nil && resp[0].TenantID != nil {
					assert.Equal(t, *tc.expectedTenantOrg, resp[0].Tenant.Org)
				}
			} else {
				if len(resp) > 0 {
					assert.Nil(t, resp[0].InfrastructureProvider)
					assert.Nil(t, resp[0].Site)
					assert.Nil(t, resp[0].Tenant)
				}

				for _, apiIpb := range resp {
					assert.Equal(t, 2, len(apiIpb.StatusHistory))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

			if tc.expectedIpam {
				if tc.expectFGPerfUsage != nil {
					assert.NotNil(t, resp[0].UsageStats)

					assert.Equal(t, int(resp[0].UsageStats.AcquiredIPs), int(tc.expectFGPerfUsage.AcquiredIPs))
					assert.Equal(t, int(resp[0].UsageStats.AcquiredPrefixes), int(tc.expectFGPerfUsage.AcquiredPrefixes))
					assert.Equal(t, int(resp[0].UsageStats.AvailableIPs), int(tc.expectFGPerfUsage.AvailableIPs))
					assert.Equal(t, len(resp[0].UsageStats.AvailablePrefixes), len(tc.expectFGPerfUsage.AvailablePrefixes))
					assert.Equal(t, int(resp[0].UsageStats.AvailableSmallestPrefixes), int(tc.expectFGPerfUsage.AvailableSmallestPrefixes))
				}

				if tc.expectPerfUsage != nil {
					assert.NotNil(t, resp[1].UsageStats)

					assert.Equal(t, int(resp[1].UsageStats.AcquiredIPs), int(tc.expectPerfUsage.AcquiredIPs))
					assert.Equal(t, int(resp[1].UsageStats.AcquiredPrefixes), int(tc.expectPerfUsage.AcquiredPrefixes))
					assert.Equal(t, int(resp[1].UsageStats.AvailableIPs), int(tc.expectPerfUsage.AvailableIPs))
					assert.Equal(t, len(resp[1].UsageStats.AvailablePrefixes), len(tc.expectPerfUsage.AvailablePrefixes))
					assert.Equal(t, int(resp[1].UsageStats.AvailableSmallestPrefixes), int(tc.expectPerfUsage.AvailableSmallestPrefixes))
				}
			}
		})
	}
}

func TestDerivedIPBlockHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()

	testIPBlockSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipOrg5 := "test-ip-org-5"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg5}, ipRoles)
	ipuv := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg5}, ipvRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testIPBlockBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	ip3 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp3", ipOrg3, ipu)
	assert.NotNil(t, ip3)

	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)
	site3 := testIPBlockBuildSite(t, dbSession, ip3, "testSite3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site3)

	tn := testIPBlockBuildTenant(t, dbSession, "testTenant", tnOrg1, tnu)
	assert.NotNil(t, tn)
	tn2 := testIPBlockBuildTenant(t, dbSession, "testTenant2", tnOrg2, tnu)
	assert.NotNil(t, tn2)

	// Parent IPBlocks
	parentIpb1 := testIPBlockBuildIPBlock(t, dbSession, "test1", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	assert.NotNil(t, parentIpb1)
	testIPBlockBuildStatusDetail(t, dbSession, parentIpb1.ID.String(), cdbm.IPBlockStatusReady)

	parentIpb2 := testIPBlockBuildIPBlock(t, dbSession, "test2", site2, ip2, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	assert.NotNil(t, parentIpb2)
	testIPBlockBuildStatusDetail(t, dbSession, parentIpb2.ID.String(), cdbm.IPBlockStatusReady)

	parentIpb3 := testIPBlockBuildIPBlock(t, dbSession, "test3", site3, ip3, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	assert.NotNil(t, parentIpb3)
	testIPBlockBuildStatusDetail(t, dbSession, parentIpb3.ID.String(), cdbm.IPBlockStatusReady)
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site3, ip3, tn, "192.168.5.0", 24, cdbm.IPBlockStatusReady, ipu)

	// Child IPBlocks
	childIpb1 := testIPBlockBuildIPBlock(t, dbSession, "test3", site, ip, cutil.GetPtr(tn.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusError, ipu)
	assert.NotNil(t, childIpb1)
	testIPBlockBuildStatusDetail(t, dbSession, childIpb1.ID.String(), cdbm.IPBlockStatusPending)

	childIpb2 := testIPBlockBuildIPBlock(t, dbSession, "test3", site2, ip2, cutil.GetPtr(tn2.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, childIpb2)
	testIPBlockBuildStatusDetail(t, dbSession, childIpb2.ID.String(), cdbm.IPBlockStatusPending)

	childIpb4 := testIPBlockBuildIPBlock(t, dbSession, "test5", site, ip, cutil.GetPtr(tn.ID), cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 18, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	assert.NotNil(t, childIpb4)
	testIPBlockBuildStatusDetail(t, dbSession, childIpb4.ID.String(), cdbm.IPBlockStatusPending)

	alloc1 := testIPBlockBuildAllocation(t, dbSession, site, tn, "testAlloc1", ipu)
	allocConstraint1 := testIPBlockBuildAllocationConstraint(t, dbSession, alloc1.ID, cdbm.AllocationResourceTypeIPBlock, parentIpb1.ID, cdbm.AllocationConstraintTypeOnDemand, 10, cutil.GetPtr(childIpb1.ID), ipu.ID)
	assert.NotNil(t, allocConstraint1)

	allocConstraint2 := testIPBlockBuildAllocationConstraint(t, dbSession, alloc1.ID, cdbm.AllocationResourceTypeIPBlock, parentIpb1.ID, cdbm.AllocationConstraintTypeOnDemand, 10, cutil.GetPtr(childIpb4.ID), ipu.ID)
	assert.NotNil(t, allocConstraint2)

	alloc2 := testIPBlockBuildAllocation(t, dbSession, site2, tn, "testAlloc2", ipu)
	allocConstraint3 := testIPBlockBuildAllocationConstraint(t, dbSession, alloc2.ID, cdbm.AllocationResourceTypeIPBlock, parentIpb2.ID, cdbm.AllocationConstraintTypeOnDemand, 10, cutil.GetPtr(childIpb2.ID), ipu.ID)
	assert.NotNil(t, allocConstraint3)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		ipbID                             string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		queryIncludeRelations3            *string
		querySearch                       *string
		queryStatus                       *string
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.IPBlock
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		expectedTenantOrg                 *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			ipbID:          parentIpb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			ipbID:          parentIpb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when infrastructure provider not found for org",
			reqOrgName:     ipOrg5,
			user:           ipu,
			ipbID:          parentIpb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipblock id is invalid uuid",
			reqOrgName:     ipOrg1,
			user:           ipu,
			ipbID:          "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipblock id not found",
			reqOrgName:     ipOrg1,
			user:           ipu,
			ipbID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when infrastructure provider in org doesnt match infrastructure provider in ipblock",
			reqOrgName:     ipOrg1,
			user:           ipu,
			ipbID:          parentIpb2.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when dervied ipblock provided as parent ipblock",
			reqOrgName:     ipOrg1,
			user:           ipu,
			ipbID:          childIpb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "Tenant SitePrefix returns not found as a parent",
			reqOrgName:     ipOrg3,
			user:           ipu,
			ipbID:          tenantSitePrefix.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:                   "success when valid parent id with two derived blocks are specified specified",
			reqOrgName:             ipOrg1,
			user:                   ipu,
			ipbID:                  parentIpb1.ID.String(),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            2,
			expectedTotal:          cutil.GetPtr(2),
			queryIncludeRelations1: cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2: cutil.GetPtr(cdbm.SiteRelationName),
			queryIncludeRelations3: cutil.GetPtr(cdbm.TenantRelationName),
			pageNumber:             cutil.GetPtr(1),
			pageSize:               cutil.GetPtr(2),
			orderBy:                cutil.GetPtr("NAME_DESC"),
			verifyChildSpanner:     true,
		},
		{
			name:               "success when user has Provider viewer role",
			reqOrgName:         ipOrg1,
			user:               ipuv,
			ipbID:              parentIpb1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        2,
			expectedTotal:      cutil.GetPtr(2),
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(2),
			orderBy:            cutil.GetPtr("NAME_DESC"),
			verifyChildSpanner: true,
		},
		{
			name:               "success when valid parent id with one derived blocks are specified specified",
			reqOrgName:         ipOrg2,
			user:               ipu,
			ipbID:              parentIpb2.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        1,
			expectedTotal:      cutil.GetPtr(1),
			verifyChildSpanner: true,
		},
		{
			name:               "success when valid parent id with zero derived blocks are specified specified",
			reqOrgName:         ipOrg3,
			user:               ipu,
			ipbID:              parentIpb3.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        0,
			verifyChildSpanner: true,
		},
		{
			name:               "success when valid parent id with zero derived blocks are specified specified with query search filter",
			reqOrgName:         ipOrg1,
			user:               ipu,
			ipbID:              parentIpb1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        1,
			querySearch:        cutil.GetPtr("test3"),
			expectedFirstEntry: childIpb2,
			verifyChildSpanner: true,
		},
		{
			name:               "success when valid parent id with zero derived blocks are specified specified with status filter",
			reqOrgName:         ipOrg1,
			user:               ipu,
			ipbID:              parentIpb1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        1,
			queryStatus:        cutil.GetPtr(cdbm.IPBlockStatusError),
			expectedFirstEntry: childIpb1,
			verifyChildSpanner: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {

			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.queryStatus != nil {
				q.Add("status", *tc.queryStatus)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}
			if tc.queryIncludeRelations3 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations3)
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

			path := fmt.Sprintf("/v2/org/%s/nico/ipblock/%s/derived?%s", tc.reqOrgName, tc.ipbID, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.ipbID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			gaipbh := GetAllDerivedIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}

			err := gaipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			if tc.expectedErr {
				return
			}

			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)

			resp := []model.APIIPBlock{}
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
				assert.Equal(t, tc.expectedFirstEntry.Name, resp[0].Name)
			}

			if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil {
				if tc.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProvider.Org)
				}
				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
				if tc.expectedTenantOrg != nil {
					assert.Equal(t, *tc.expectedTenantOrg, resp[0].Tenant.Org)
				}
			} else {
				if len(resp) > 0 {
					assert.Nil(t, resp[0].InfrastructureProvider)
					assert.Nil(t, resp[0].Site)
					assert.Nil(t, resp[0].Tenant)
				}

				for _, apiIpb := range resp {
					assert.Equal(t, 1, len(apiIpb.StatusHistory))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestIPBlockHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()

	testIPBlockSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testIPBlockBuildUser(t, dbSession, "TestIPBlockHandler_Delete", []string{ipOrg1, ipOrg2, ipOrg3, tnOrg1, tnOrg2}, orgRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, user)
	assert.NotNil(t, ip2)
	ip3 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp3", ipOrg3, user)
	assert.NotNil(t, ip3)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site2)
	site3 := testIPBlockBuildSite(t, dbSession, ip3, "testSite3", cdbm.SiteStatusRegistered, true, user)
	assert.NotNil(t, site3)

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testDel1", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb1)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "testDel2", site2, ip2, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb2)
	parentPref2, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref2)

	ipb3 := testIPBlockBuildIPBlock(t, dbSession, "testDel3", site3, ip3, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb3)

	tn := testIPBlockBuildTenant(t, dbSession, "testTenant", tnOrg1, user)
	assert.NotNil(t, tn)
	alloc := testIPBlockBuildAllocation(t, dbSession, site3, tn, "testAlloc", user)
	allocConstraint := testIPBlockBuildAllocationConstraint(t, dbSession, alloc.ID, cdbm.AllocationResourceTypeIPBlock, ipb3.ID, cdbm.AllocationConstraintTypeOnDemand, 10, nil, user.ID)
	assert.NotNil(t, allocConstraint)

	ipb4 := testIPBlockBuildIPBlock(t, dbSession, "testDel4", site3, ip3, &tn.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.3.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, user)
	assert.NotNil(t, ipb4)
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site3, ip3, tn, "192.168.4.0", 24, cdbm.IPBlockStatusPending, user)
	coreLinkedRoot := testIPBlockBuildIPBlock(t, dbSession, "core-linked-root", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.2.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, user)
	_, err = cdbm.NewIPBlockDAO(dbSession).LinkSitePrefix(ctx, nil, coreLinkedRoot.ID, uuid.New())
	require.NoError(t, err)
	coreLinkedAllocation := testIPBlockBuildAllocation(t, dbSession, site, tn, "coreLinkedAllocation", user)
	coreLinkedConstraint := testIPBlockBuildAllocationConstraint(t, dbSession, coreLinkedAllocation.ID, cdbm.AllocationResourceTypeIPBlock, coreLinkedRoot.ID, cdbm.AllocationConstraintTypeOnDemand, 10, nil, user.ID)
	require.NotNil(t, coreLinkedConstraint)
	coreLinkedPrefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, coreLinkedRoot.Prefix, coreLinkedRoot.PrefixLength, coreLinkedRoot.RoutingType, coreLinkedRoot.InfrastructureProviderID.String(), coreLinkedRoot.SiteID.String())
	require.NoError(t, err)
	require.NotNil(t, coreLinkedPrefix)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		ipbID              string
		ipb                *cdbm.IPBlock
		expectedErr        bool
		expectedStatus     int
		expectedMessage    string
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when ipblock id is invalid uuid",
			reqOrgName:     ipOrg1,
			user:           user,
			ipbID:          "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have infrastructure provider",
			reqOrgName:     ipOrg3,
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when specified ipblock doesnt exist",
			reqOrgName:     ipOrg1,
			user:           user,
			ipbID:          uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when org's infrastructure provider does not match ipblock's infrastructure provider",
			reqOrgName:     ipOrg2,
			user:           user,
			ipbID:          ipb1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when this is a derived ipBlock with non-nil tenantID",
			reqOrgName:     ipOrg3,
			user:           user,
			ipbID:          ipb4.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "Tenant SitePrefix returns not found",
			reqOrgName:     ipOrg3,
			user:           user,
			ipbID:          tenantSitePrefix.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when allocations exist for ipblock",
			reqOrgName:     ipOrg3,
			user:           user,
			ipbID:          ipb3.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:            "Core-linked Site fabric IP Block returns conflict before allocation checks",
			reqOrgName:      ipOrg1,
			user:            user,
			ipbID:           coreLinkedRoot.ID.String(),
			expectedErr:     true,
			expectedStatus:  http.StatusConflict,
			expectedMessage: "This IP Block is linked to an OperatorManaged SitePrefix; remove the corresponding prefix from Core's site_fabric_prefixes configuration instead",
		},
		{
			name:               "success case 1",
			reqOrgName:         ipOrg1,
			user:               user,
			ipbID:              ipb1.ID.String(),
			ipb:                ipb1,
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name:           "success case 2",
			reqOrgName:     ipOrg2,
			user:           user,
			ipbID:          ipb2.ID.String(),
			ipb:            ipb2,
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
			values := []string{tc.reqOrgName, tc.ipbID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			dipbh := DeleteIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := dipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if tc.expectedMessage != "" {
				response := struct {
					Message string `json:"message"`
				}{}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, tc.expectedMessage, response.Message)
			}
			if !tc.expectedErr && rec.Code != http.StatusAccepted {
				rsp := &model.APIIPBlock{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
			}
			// validate ipam for ipblock got deleted
			if rec.Code == http.StatusAccepted {
				ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
				ipamer := cipam.NewWithStorage(ipamStorage)
				ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, tc.ipb.RoutingType, tc.ipb.InfrastructureProviderID.String(), tc.ipb.SiteID.String()))
				pref := ipamer.PrefixFrom(ctx, ipam.GetCidrForIPBlock(ctx, tc.ipb.Prefix, tc.ipb.PrefixLength))
				assert.Nil(t, pref)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

	type raceFixture struct {
		ipBlock *cdbm.IPBlock
		prefix  *cipam.Prefix
	}

	createFixture := func(t *testing.T, name, prefix string) raceFixture {
		t.Helper()
		ipBlock := testIPBlockBuildIPBlock(
			t,
			dbSession,
			name,
			site,
			ip,
			nil,
			cdbm.IPBlockRoutingTypeDatacenterOnly,
			prefix,
			24,
			cdbm.IPBlockProtocolVersionV4,
			false,
			cdbm.IPBlockStatusReady,
			user,
		)
		ipamPrefix, err := ipam.CreateIpamEntryForIPBlock(
			ctx,
			ipamStorage,
			ipBlock.Prefix,
			ipBlock.PrefixLength,
			ipBlock.RoutingType,
			ipBlock.InfrastructureProviderID.String(),
			ipBlock.SiteID.String(),
		)
		require.NoError(t, err)
		require.NotNil(t, ipamPrefix)
		return raceFixture{ipBlock: ipBlock, prefix: ipamPrefix}
	}

	startDelete := func(t *testing.T, raceCtx context.Context, cancel context.CancelFunc, id uuid.UUID) (*httptest.ResponseRecorder, <-chan error, *bool) {
		t.Helper()
		e := echo.New()
		req := httptest.NewRequest(http.MethodPost, "/", nil)
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		rec := httptest.NewRecorder()
		ec := e.NewContext(req, rec)
		ec.SetParamNames("orgName", "id")
		ec.SetParamValues(ipOrg1, id.String())
		ec.Set("user", user)
		requestCtx := context.WithValue(raceCtx, otelecho.TracerKey, tracer) //nolint:staticcheck // Middleware owns the context key.
		ec.SetRequest(ec.Request().WithContext(requestCtx))

		done := make(chan error, 1)
		go func() {
			done <- (DeleteIPBlockHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}).Handle(ec)
		}()

		drained := false
		t.Cleanup(func() {
			cancel()
			if drained {
				return
			}
			select {
			case <-done:
			case <-time.After(5 * time.Second):
				t.Error("provider deletion handler did not stop during cleanup")
			}
		})
		return rec, done, &drained
	}

	t.Run("rejects an allocation that commits while deletion waits", func(t *testing.T) {
		fixture := createFixture(t, "allocation-delete-race", "192.168.5.0")
		raceRoot := fixture.ipBlock
		raceAllocation := testIPBlockBuildAllocation(t, dbSession, site, tn, "allocationDeleteRace", user)

		raceCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		allocationTx, err := cdb.BeginTx(raceCtx, dbSession, nil)
		require.NoError(t, err)
		allocationCommitted := false
		defer func() {
			if !allocationCommitted {
				_ = allocationTx.Rollback()
			}
		}()

		var allocationBackendPID int
		err = allocationTx.GetBunTx().NewSelect().
			ColumnExpr("pg_backend_pid()").
			Scan(raceCtx, &allocationBackendPID)
		require.NoError(t, err)
		allocationStorage := ipam.NewIpamStorage(dbSession.DB, allocationTx.GetBunTx())
		allocatedChild, err := ipam.CreateChildIpamEntryForIPBlock(
			raceCtx,
			allocationTx,
			dbSession,
			allocationStorage,
			raceRoot,
			28,
		)
		require.NoError(t, err)
		_, err = cdbm.NewAllocationConstraintDAO(dbSession).Create(
			raceCtx,
			allocationTx,
			cdbm.AllocationConstraintCreateInput{
				AllocationID:    raceAllocation.ID,
				ResourceType:    cdbm.AllocationResourceTypeIPBlock,
				ResourceTypeID:  raceRoot.ID,
				ConstraintType:  cdbm.AllocationConstraintTypeOnDemand,
				ConstraintValue: 28,
				CreatedBy:       user.ID,
			},
		)
		require.NoError(t, err)

		rec, handlerDone, handlerDrained := startDelete(t, raceCtx, cancel, raceRoot.ID)

		require.Eventually(t, func() bool {
			var waiters int
			queryErr := dbSession.DB.NewSelect().
				ColumnExpr("count(*)").
				TableExpr("pg_catalog.pg_stat_activity").
				Where("? = ANY(pg_blocking_pids(pid))", allocationBackendPID).
				Scan(raceCtx, &waiters)
			return queryErr == nil && waiters > 0
		}, 5*time.Second, 10*time.Millisecond, "IP Block deletion did not wait for the lock on the allocation parent row")

		require.NoError(t, allocationTx.Commit())
		allocationCommitted = true
		select {
		case err = <-handlerDone:
			*handlerDrained = true
			require.NoError(t, err)
		case <-raceCtx.Done():
			t.Fatalf("IP Block deletion did not resume after allocation commit: %v", raceCtx.Err())
		}
		assert.Equal(t, http.StatusBadRequest, rec.Code)
		assert.Contains(t, rec.Body.String(), "1 Allocations exist for IP Block, unable to delete")

		_, err = cdbm.NewIPBlockDAO(dbSession).GetByID(ctx, nil, raceRoot.ID, nil)
		require.NoError(t, err)
		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx,
			raceRoot.RoutingType,
			raceRoot.InfrastructureProviderID.String(),
			raceRoot.SiteID.String(),
		))
		require.NotNil(t, ipamer.PrefixFrom(ctx, fixture.prefix.Cidr))
		require.NotNil(t, ipamer.PrefixFrom(ctx, allocatedChild.Cidr))
	})

	t.Run("orders Core links with provider deletion", func(t *testing.T) {
		assertIPAMRecord := func(t *testing.T, fixture raceFixture, expected bool) {
			t.Helper()
			ipamer := cipam.NewWithStorage(ipamStorage)
			ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
				ctx,
				fixture.ipBlock.RoutingType,
				fixture.ipBlock.InfrastructureProviderID.String(),
				fixture.ipBlock.SiteID.String(),
			))
			if expected {
				require.NotNil(t, ipamer.PrefixFrom(ctx, fixture.prefix.Cidr))
				return
			}
			require.Nil(t, ipamer.PrefixFrom(ctx, fixture.prefix.Cidr))
		}

		raceCases := []struct {
			name   string
			prefix string
			run    func(t *testing.T, fixture raceFixture)
		}{
			{
				name:   "link commits first",
				prefix: "192.168.6.0",
				run: func(t *testing.T, fixture raceFixture) {
					raceCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
					defer cancel()

					linkTx, err := cdb.BeginTx(raceCtx, dbSession, nil)
					require.NoError(t, err)
					linkCommitted := false
					defer func() {
						if !linkCommitted {
							assert.NoError(t, linkTx.Rollback())
						}
					}()

					var linkBackendPID int
					err = linkTx.GetBunTx().NewSelect().
						ColumnExpr("pg_backend_pid()").
						Scan(raceCtx, &linkBackendPID)
					require.NoError(t, err)
					// Keep the link uncommitted so the provider delete reaches the
					// same row only after the link already owns it.
					sitePrefixID := uuid.New()
					_, err = cdbm.NewIPBlockDAO(dbSession).LinkSitePrefix(
						raceCtx,
						linkTx,
						fixture.ipBlock.ID,
						sitePrefixID,
					)
					require.NoError(t, err)

					rec, deleteDone, deleteDrained := startDelete(t, raceCtx, cancel, fixture.ipBlock.ID)
					require.Eventually(t, func() bool {
						var waiters int
						queryErr := dbSession.DB.NewSelect().
							ColumnExpr("count(*)").
							TableExpr("pg_catalog.pg_stat_activity").
							Where("datname = current_database()").
							Where("state = 'active'").
							Where("wait_event_type = 'Lock'").
							Where("query ILIKE '%ip_block%'").
							Where("query ILIKE '%FOR UPDATE%'").
							Where("? = ANY(pg_blocking_pids(pid))", linkBackendPID).
							Scan(raceCtx, &waiters)
						return queryErr == nil && waiters > 0
					}, 5*time.Second, 10*time.Millisecond, "provider deletion did not wait for the Core link")

					require.NoError(t, linkTx.Commit())
					linkCommitted = true
					select {
					case err = <-deleteDone:
						*deleteDrained = true
						require.NoError(t, err)
					case <-raceCtx.Done():
						t.Fatalf("provider deletion did not resume after the Core link committed: %v", raceCtx.Err())
					}
					assert.Equal(t, http.StatusConflict, rec.Code)
					assert.Contains(t, rec.Body.String(), "linked to an OperatorManaged SitePrefix")

					linked, err := cdbm.NewIPBlockDAO(dbSession).GetByID(ctx, nil, fixture.ipBlock.ID, nil)
					require.NoError(t, err)
					require.NotNil(t, linked.SitePrefixID)
					assert.Equal(t, sitePrefixID, *linked.SitePrefixID)
					assertIPAMRecord(t, fixture, true)
				},
			},
			{
				name:   "deletion commits first",
				prefix: "192.168.7.0",
				run: func(t *testing.T, fixture raceFixture) {
					raceCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
					defer cancel()

					gateTx, err := cdb.BeginTx(raceCtx, dbSession, nil)
					require.NoError(t, err)
					gateCommitted := false
					defer func() {
						if !gateCommitted {
							assert.NoError(t, gateTx.Rollback())
						}
					}()
					// Provider deletion locks the IP Block row before querying
					// Allocation Constraints. Pause it at that query so the Core link
					// starts only after deletion owns the row.
					_, err = gateTx.GetBunTx().ExecContext(
						raceCtx,
						"LOCK TABLE allocation_constraint IN ACCESS EXCLUSIVE MODE",
					)
					require.NoError(t, err)
					var gateBackendPID int
					err = gateTx.GetBunTx().NewSelect().
						ColumnExpr("pg_backend_pid()").
						Scan(raceCtx, &gateBackendPID)
					require.NoError(t, err)

					rec, deleteDone, deleteDrained := startDelete(t, raceCtx, cancel, fixture.ipBlock.ID)
					var deleteBackendPID int
					require.Eventually(t, func() bool {
						queryErr := dbSession.DB.NewSelect().
							ColumnExpr("pid").
							TableExpr("pg_catalog.pg_stat_activity").
							Where("datname = current_database()").
							Where("state = 'active'").
							Where("wait_event_type = 'Lock'").
							Where("query ILIKE '%allocation_constraint%'").
							Where("? = ANY(pg_blocking_pids(pid))", gateBackendPID).
							OrderExpr("query_start DESC").
							Limit(1).
							Scan(raceCtx, &deleteBackendPID)
						return queryErr == nil && deleteBackendPID > 0
					}, 5*time.Second, 10*time.Millisecond, "provider deletion did not reach the Allocation Constraint check")

					linkTx, err := cdb.BeginTx(raceCtx, dbSession, nil)
					require.NoError(t, err)
					linkCtx, cancelLink := context.WithCancel(raceCtx)
					linkRolledBack := false
					linkJoined := false
					linkStarted := false
					linkDone := make(chan error, 1)
					t.Cleanup(func() {
						cancelLink()
						if linkStarted && !linkJoined {
							<-linkDone
							linkJoined = true
						}
						if !linkRolledBack {
							if rollbackErr := linkTx.Rollback(); rollbackErr != nil {
								t.Errorf("roll back Core link transaction: %v", rollbackErr)
							}
							linkRolledBack = true
						}
					})
					var linkBackendPID int
					err = linkTx.GetBunTx().NewSelect().
						ColumnExpr("pg_backend_pid()").
						Scan(raceCtx, &linkBackendPID)
					require.NoError(t, err)
					go func() {
						_, linkErr := cdbm.NewIPBlockDAO(dbSession).LinkSitePrefix(
							linkCtx,
							linkTx,
							fixture.ipBlock.ID,
							uuid.New(),
						)
						linkDone <- linkErr
					}()
					linkStarted = true
					require.Eventually(t, func() bool {
						var waiters int
						queryErr := dbSession.DB.NewSelect().
							ColumnExpr("count(*)").
							TableExpr("pg_catalog.pg_stat_activity").
							Where("pid = ?", linkBackendPID).
							Where("? = ANY(pg_blocking_pids(pid))", deleteBackendPID).
							Scan(raceCtx, &waiters)
						return queryErr == nil && waiters == 1
					}, 5*time.Second, 10*time.Millisecond, "Core link did not wait for provider deletion")

					require.NoError(t, gateTx.Commit())
					gateCommitted = true
					select {
					case err = <-deleteDone:
						*deleteDrained = true
						require.NoError(t, err)
					case <-raceCtx.Done():
						t.Fatalf("provider deletion did not resume after the test gate opened: %v", raceCtx.Err())
					}
					assert.Equal(t, http.StatusAccepted, rec.Code)

					select {
					case err = <-linkDone:
						linkJoined = true
						require.ErrorIs(t, err, cdb.ErrInvalidValue)
					case <-raceCtx.Done():
						t.Fatalf("Core link did not resume after provider deletion committed: %v", raceCtx.Err())
					}
					require.NoError(t, linkTx.Rollback())
					linkRolledBack = true

					_, err = cdbm.NewIPBlockDAO(dbSession).GetByID(ctx, nil, fixture.ipBlock.ID, nil)
					require.ErrorIs(t, err, cdb.ErrDoesNotExist)
					deleted := &cdbm.IPBlock{}
					err = dbSession.DB.NewSelect().Model(deleted).
						WhereAllWithDeleted().
						Where("ipb.id = ?", fixture.ipBlock.ID).
						Scan(ctx)
					require.NoError(t, err)
					require.NotNil(t, deleted.Deleted)
					assert.Nil(t, deleted.SitePrefixID)
					assertIPAMRecord(t, fixture, false)
				},
			},
		}

		for _, tc := range raceCases {
			t.Run(tc.name, func(t *testing.T) {
				fixture := createFixture(t, tc.name, tc.prefix)
				tc.run(t, fixture)
			})
		}
	})

	// The filtered NotFound path must leave the Tenant SitePrefix in the database.
	_, err = cdbm.NewIPBlockDAO(dbSession).GetByID(ctx, nil, tenantSitePrefix.ID, nil)
	require.NoError(t, err)
	_, err = cdbm.NewIPBlockDAO(dbSession).GetByID(ctx, nil, coreLinkedRoot.ID, nil)
	require.NoError(t, err)
	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, coreLinkedRoot.RoutingType, coreLinkedRoot.InfrastructureProviderID.String(), coreLinkedRoot.SiteID.String()))
	require.NotNil(t, ipamer.PrefixFrom(ctx, ipam.GetCidrForIPBlock(ctx, coreLinkedRoot.Prefix, coreLinkedRoot.PrefixLength)))
}
