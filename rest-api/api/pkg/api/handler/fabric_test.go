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
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	tmocks "go.temporal.io/sdk/mocks"
)

func testFabricInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testFabricBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, org, name string) *cdbm.InfrastructureProvider {
	ip := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: name,
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testFabricBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, config *cdbm.SiteConfig, status *string) *cdbm.Site {
	if status == nil {
		status = cutil.GetPtr(cdbm.SiteStatusRegistered)
	}

	st := &cdbm.Site{
		ID:                          uuid.New(),
		Name:                        name,
		Org:                         ip.Org,
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:           false,
		SerialConsoleHostname:       cutil.GetPtr("TestSshHostname"),
		Status:                      *status,
		Config:                      config,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testFabricBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string) *cdbm.User {
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

func testFabricBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID string, status string) {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID, Status: status, Message: nil})
	assert.Nil(t, err)
	assert.NotNil(t, ssd)
	assert.Equal(t, entityID, ssd.EntityID)
	assert.Equal(t, status, ssd.Status)
}

func testFabricBuildTenant(t *testing.T, dbSession *cdb.Session, org, name string) *cdbm.Tenant {
	tenant := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testFabricBuildFabric(t *testing.T, dbSession *cdb.Session, org string, id *string, ip *cdbm.InfrastructureProvider, site *cdbm.Site, status *string, isMissingOnSite bool) *cdbm.Fabric {
	if id == nil {
		id = cutil.GetPtr("IFabric1")
	}
	if status == nil {
		status = cutil.GetPtr(cdbm.FabricStatusReady)
	}
	fb := &cdbm.Fabric{
		ID:                       *id,
		Org:                      org,
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		Status:                   *status,
		IsMissingOnSite:          isMissingOnSite,
	}
	_, err := dbSession.DB.NewInsert().Model(fb).Exec(context.Background())
	assert.Nil(t, err)
	return fb
}

func TestFabricHandler_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipvRoles)

	tnOrg1 := "test-tn-org-1"
	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles1)

	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles2)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	ip2 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg2, "infraProvider2")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)
	site2 := testFabricBuildSite(t, dbSession, ip2, "testSite2", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	fb1 := testFabricBuildFabric(t, dbSession, ip1.Org, cutil.GetPtr("IFabric1"), ip1, site1, nil, false)
	assert.NotNil(t, fb1)

	fb2 := testFabricBuildFabric(t, dbSession, ip2.Org, cutil.GetPtr("IFabric2"), ip2, site2, nil, false)
	assert.NotNil(t, fb2)

	testFabricBuildStatusDetail(t, dbSession, fb1.ID, cdbm.FabricStatusReady)
	testFabricBuildStatusDetail(t, dbSession, fb2.ID, cdbm.FabricStatusReady)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		fbID                              string
		siteID                            uuid.UUID
		expectedErr                       bool
		expectedStatus                    int
		expectedID                        string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		expectedSiteName                  *string
		expectedInfrastructureProviderOrg *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			fbID:           fb1.ID,
			siteID:         site1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			fbID:           fb1.ID,
			siteID:         site1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenat or ip are not an admin",
			reqOrgName:     tnOrg1,
			user:           tnu2,
			fbID:           fb1.ID,
			siteID:         site1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when fabric id not found",
			reqOrgName:     ipOrg1,
			user:           ipu,
			fbID:           "IFabric4",
			siteID:         site1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when fabric associated with site not match with requested",
			reqOrgName:     ipOrg1,
			user:           ipu,
			fbID:           fb1.ID,
			siteID:         site2.ID,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:                              "success case when user has Provider admin role",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			fbID:                              fb1.ID,
			siteID:                            site1.ID,
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        fb1.ID,
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.SiteRelationName),
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip1.Org),
			expectedSiteName:                  cutil.GetPtr(site1.Name),
			verifyChildSpanner:                true,
		},
		{
			name:                              "success case when user has Provider viewer role",
			reqOrgName:                        ipOrg1,
			user:                              ipuv,
			fbID:                              fb1.ID,
			siteID:                            site1.ID,
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        fb1.ID,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip1.Org),
			expectedSiteName:                  cutil.GetPtr(site1.Name),
		},
		{
			name:           "success case for Tenant when they have site association",
			reqOrgName:     tnOrg1,
			user:           tnu1,
			fbID:           fb1.ID,
			siteID:         site1.ID,
			expectedErr:    false,
			expectedID:     fb1.ID,
			expectedStatus: http.StatusOK,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			q := req.URL.Query()

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
			names := []string{"orgName", "siteId", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.siteID.String(), tc.fbID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			fbh := GetFabricHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := fbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIFabric{}
				b := rec.Body.Bytes()
				fmt.Println(string(b))
				err := json.Unmarshal(b, rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedID, rsp.ID)
				assert.Equal(t, tc.siteID.String(), rsp.SiteID)

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProvider.Org)
					}
					if tc.expectedSiteName != nil {
						assert.Equal(t, *tc.expectedSiteName, rsp.Site.Name)
					}
				} else {
					assert.Nil(t, rsp.InfrastructureProvider)
					assert.Nil(t, rsp.Site)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestFabricHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testFabricInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipvRoles)

	tnOrg1 := "test-tn-org-1"
	tnRoles1 := []string{authz.TenantAdminRole}
	tnRoles2 := []string{"NICO_TENANT_NONADMIN"}

	tnu1 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, ipOrg1}, tnRoles1)

	tnu2 := testFabricBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles2)
	assert.NotNil(t, tnu2)

	ip1 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	ip2 := testFabricBuildInfrastructureProvider(t, dbSession, ipOrg2, "infraProvider2")
	site1 := testFabricBuildSite(t, dbSession, ip1, "testSite1", nil, nil)
	site2 := testFabricBuildSite(t, dbSession, ip2, "testSite2", nil, nil)

	tn1 := testFabricBuildTenant(t, dbSession, tnOrg1, "testTenant1")
	assert.NotNil(t, tn1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, site1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	totalCount := 30

	fbs := []cdbm.Fabric{}

	for i := 0; i < totalCount; i++ {
		var ip *cdbm.InfrastructureProvider
		var site *cdbm.Site

		if i%2 == 0 {
			ip = ip1
			site = site1
		} else {
			ip = ip2
			site = site2
		}

		fb := testFabricBuildFabric(t, dbSession, ip.Org, cutil.GetPtr(fmt.Sprintf("IFabric%d", i)), ip, site, nil, false)
		assert.NotNil(t, fb)

		if i%2 == 0 {
			testFabricBuildStatusDetail(t, dbSession, fb.ID, cdbm.FabricStatusReady)
		} else {
			testFabricBuildStatusDetail(t, dbSession, fb.ID, cdbm.FabricStatusPending)
		}
		fbs = append(fbs, *fb)
	}

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		querySiteID                       string
		queryID                           *string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.Fabric
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when User is not found in request context",
			reqOrgName:     ipOrg1,
			querySiteID:    site1.ID.String(),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when User does not have org membership",
			reqOrgName:     "SomeOrg",
			querySiteID:    site1.ID.String(),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when Infrastructure Provider is not set up for org",
			reqOrgName:     ipOrg3,
			querySiteID:    site1.ID.String(),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when Site ID specified in query is an invalid UUID",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when non-existent Site ID is specified in query",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Site's Provider does not match org's Provider",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    site2.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "success case when Site ID specified in query",
			reqOrgName:         ipOrg1,
			user:               ipu,
			querySiteID:        site1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        totalCount / 2,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			verifyChildSpanner: true,
		},
		{
			name:               "success case when user has Provider viewer role",
			reqOrgName:         ipOrg1,
			user:               ipuv,
			querySiteID:        site1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        totalCount / 2,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			verifyChildSpanner: true,
		},
		{
			name:               "success case with tenant admin and Site ID specified in query",
			reqOrgName:         tnOrg1,
			user:               tnu1,
			querySiteID:        site1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        0,
			expectedTotal:      cutil.GetPtr(0),
			verifyChildSpanner: true,
		},
		{
			name:               "success when pagination params are specified",
			reqOrgName:         ipOrg1,
			user:               ipu,
			querySiteID:        site1.ID.String(),
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(10),
			orderBy:            cutil.GetPtr("CREATED_DESC"),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        10,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectedFirstEntry: &fbs[28],
		},
		{
			name:           "failure when invalid pagination params are specified",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    site1.ID.String(),
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:                              "success when include relation with site",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			querySiteID:                       site1.ID.String(),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       totalCount / 2,
			expectedTotal:                     cutil.GetPtr(totalCount / 2),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.SiteRelationName),
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip1.Org),
			expectedSiteName:                  cutil.GetPtr(site1.Name),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.queryID != nil {
				q.Add("id", *tc.queryID)
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

			path := fmt.Sprintf("/v2/org/%s/nico/site/%s/fabric?%s", tc.reqOrgName, tc.querySiteID, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteId")
			ec.SetParamValues(tc.reqOrgName, tc.querySiteID)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			gafh := GetAllFabricHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := gafh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			if tc.expectedErr {
				return
			}

			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)

			resp := []model.APIFabric{}
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
				assert.Equal(t, tc.expectedFirstEntry.ID, resp[0].ID)
			}

			if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
				if tc.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProvider.Org)
				}
				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
			} else if len(resp) > 0 {
				assert.Nil(t, resp[0].InfrastructureProvider)
				assert.Nil(t, resp[0].Site)
			}

			for _, apim := range resp {
				assert.Equal(t, 1, len(apim.StatusHistory))
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
