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
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
)

// ipxeTemplateTestFixture wires up a provider, one of its sites, a tenant with
// access to that site, and two templates: one available at the site (via an
// IpxeTemplateSiteAssociation) and one that exists but is not available anywhere.
type ipxeTemplateTestFixture struct {
	dbSession  *cdb.Session
	ipOrg      string
	tnOrg      string
	emptyIPOrg string
	provider   *cdbm.InfrastructureProvider
	site       *cdbm.Site
	tenant     *cdbm.Tenant
	availTmpl  *cdbm.IpxeTemplate
	orphanTmpl *cdbm.IpxeTemplate
	provUser   *cdbm.User
	tnUser     *cdbm.User
}

func buildIpxeTemplateFixture(t *testing.T) (*ipxeTemplateTestFixture, func()) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	common.TestSetupSchema(t, dbSession)

	f := &ipxeTemplateTestFixture{
		ipOrg:      "ipxet-ip-org",
		tnOrg:      "ipxet-tn-org",
		emptyIPOrg: "ipxet-empty-ip-org",
	}

	// A provider admin whose org owns the site, plus an empty provider org to
	// exercise the "no associated site" path.
	f.provUser = testMachineBuildUser(t, dbSession, uuid.NewString(), []string{f.ipOrg, f.emptyIPOrg}, []string{authz.ProviderAdminRole})
	f.provider = testMachineBuildInfrastructureProvider(t, dbSession, f.ipOrg, "ipxet-provider")
	testMachineBuildInfrastructureProvider(t, dbSession, f.emptyIPOrg, "ipxet-empty-provider")
	f.site = testMachineBuildSite(t, dbSession, f.provider, "ipxet-site", cdbm.SiteStatusRegistered)

	// A tenant admin with a tenant account on the provider's site.
	f.tnUser = testMachineBuildUser(t, dbSession, uuid.NewString(), []string{f.tnOrg}, []string{authz.TenantAdminRole})
	f.tenant = testMachineBuildTenant(t, dbSession, f.tnOrg, "ipxet-tenant")
	testBuildTenantSiteAssociation(t, dbSession, f.tnOrg, f.tenant.ID, f.site.ID, f.tnUser.ID)

	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)
	availTmpl, err := templateDAO.Create(ctx, nil, cdbm.IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "ipxet-available",
		Template:   "#!ipxe\n",
		Visibility: "Public",
	})
	require.NoError(t, err)
	f.availTmpl = availTmpl

	orphanTmpl, err := templateDAO.Create(ctx, nil, cdbm.IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "ipxet-orphan",
		Template:   "#!ipxe\n",
		Visibility: "Public",
	})
	require.NoError(t, err)
	f.orphanTmpl = orphanTmpl

	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)
	_, err = itsaDAO.Create(ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: availTmpl.ID, SiteID: f.site.ID})
	require.NoError(t, err)

	cleanup := func() { dbSession.Close() }
	// Store the session on the fixture indirectly via closures used by the tests.
	f.dbSession = dbSession
	return f, cleanup
}

func TestIpxeTemplateHandler_GetAll(t *testing.T) {
	f, cleanup := buildIpxeTemplateFixture(t)
	defer cleanup()

	ctx := context.Background()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		siteID             *uuid.UUID
		queryParams        url.Values
		expectedErr        bool
		expectedStatus     int
		expectedNames      []string
		expectedPageNumber int
		expectedPageSize   int
		expectedTotal      int
	}{
		{
			name:           "error when user not in request context",
			reqOrgName:     f.ipOrg,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not a member of org",
			reqOrgName:     "SomeOtherOrg",
			user:           f.provUser,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "provider admin sees templates available at owned sites",
			reqOrgName:         f.ipOrg,
			user:               f.provUser,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedNames:      []string{f.availTmpl.Name},
			expectedPageNumber: 1,
			expectedPageSize:   cdbp.DefaultLimit,
			expectedTotal:      1,
		},
		{
			name:               "tenant admin sees templates available at accessible sites",
			reqOrgName:         f.tnOrg,
			user:               f.tnUser,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedNames:      []string{f.availTmpl.Name},
			expectedPageNumber: 1,
			expectedPageSize:   cdbp.DefaultLimit,
			expectedTotal:      1,
		},
		{
			name:               "provider admin with explicit authorized siteId",
			reqOrgName:         f.ipOrg,
			user:               f.provUser,
			siteID:             &f.site.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedNames:      []string{f.availTmpl.Name},
			expectedPageNumber: 1,
			expectedPageSize:   cdbp.DefaultLimit,
			expectedTotal:      1,
		},
		{
			name:           "provider admin with unauthorized siteId is forbidden",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			siteID:         cutilPtrUUID(uuid.New()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when siteId is empty",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			queryParams:    url.Values{"siteId": {""}},
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when unknown query parameter is specified",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			queryParams:    url.Values{"foo": {"bar"}},
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "provider org without any site returns forbidden",
			reqOrgName:     f.emptyIPOrg,
			user:           f.provUser,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tc2 := range tests {
		t.Run(tc2.name, func(t *testing.T) {
			e := echo.New()
			q := url.Values{}
			if tc2.queryParams != nil {
				q = tc2.queryParams
			} else if tc2.siteID != nil {
				q.Add("siteId", tc2.siteID.String())
			}
			path := fmt.Sprintf("/v2/org/%s/nico/ipxe-template?%s", tc2.reqOrgName, q.Encode())
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc2.reqOrgName)
			if tc2.user != nil {
				ec.Set("user", tc2.user)
			}
			ec.SetRequest(ec.Request().WithContext(ctx))

			h := NewGetAllIpxeTemplateHandler(f.dbSession, tc, cfg)
			err := h.Handle(ec)
			assert.Nil(t, err)
			require.Equal(t, tc2.expectedStatus, rec.Code)
			assert.Equal(t, tc2.expectedErr, rec.Code != http.StatusOK)

			if !tc2.expectedErr {
				rsp := []model.APIIpxeTemplate{}
				err := json.Unmarshal(rec.Body.Bytes(), &rsp)
				assert.Nil(t, err)
				gotNames := map[string]bool{}
				for _, tmpl := range rsp {
					gotNames[tmpl.Name] = true
				}
				for _, want := range tc2.expectedNames {
					assert.True(t, gotNames[want], "expected template %q in response", want)
				}
				// The orphan template must never be returned.
				assert.False(t, gotNames[f.orphanTmpl.Name], "orphan template must not be visible")

				paginationHeader := rec.Header().Get(pagination.ResponseHeaderName)
				assert.NotEmpty(t, paginationHeader)
				var pageResp pagination.PageResponse
				err = json.Unmarshal([]byte(paginationHeader), &pageResp)
				require.NoError(t, err)
				assert.Equal(t, tc2.expectedPageNumber, pageResp.PageNumber)
				assert.Equal(t, tc2.expectedPageSize, pageResp.PageSize)
				assert.Equal(t, tc2.expectedTotal, pageResp.Total)
			}
		})
	}
}

func TestIpxeTemplateHandler_Get(t *testing.T) {
	f, cleanup := buildIpxeTemplateFixture(t)
	defer cleanup()

	ctx := context.Background()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		templateID     string
		expectedErr    bool
		expectedStatus int
	}{
		{
			name:           "error when user not in request context",
			reqOrgName:     f.ipOrg,
			user:           nil,
			templateID:     f.availTmpl.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not a member of org",
			reqOrgName:     "SomeOtherOrg",
			user:           f.provUser,
			templateID:     f.availTmpl.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when template id is not a uuid",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			templateID:     "not-a-uuid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when template does not exist",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			templateID:     uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "forbidden when template not available at any accessible site",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			templateID:     f.orphanTmpl.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin can retrieve available template",
			reqOrgName:     f.ipOrg,
			user:           f.provUser,
			templateID:     f.availTmpl.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
		{
			name:           "tenant admin can retrieve available template",
			reqOrgName:     f.tnOrg,
			user:           f.tnUser,
			templateID:     f.availTmpl.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
	}

	for _, tc2 := range tests {
		t.Run(tc2.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc2.reqOrgName, tc2.templateID)
			if tc2.user != nil {
				ec.Set("user", tc2.user)
			}
			ec.SetRequest(ec.Request().WithContext(ctx))

			h := NewGetIpxeTemplateHandler(f.dbSession, tc, cfg)
			err := h.Handle(ec)
			assert.Nil(t, err)
			require.Equal(t, tc2.expectedStatus, rec.Code)
			assert.Equal(t, tc2.expectedErr, rec.Code != http.StatusOK)

			if !tc2.expectedErr {
				rsp := &model.APIIpxeTemplate{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc2.templateID, rsp.ID)
			}
		})
	}
}

func cutilPtrUUID(id uuid.UUID) *uuid.UUID { return &id }
