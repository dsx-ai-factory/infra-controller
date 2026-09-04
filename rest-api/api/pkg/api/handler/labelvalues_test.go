// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	modelutil "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestGetAllMachineLabelHandlers(t *testing.T) {
	ctx := context.Background()
	dbSession := testExpectedMachineInitDB(t)
	defer dbSession.Close()
	org := "label-values-org"
	provider, site := testExpectedMachineSetupTestData(t, dbSession, org)
	filteredSite := &cdbm.Site{ID: uuid.New(), Name: "filtered-site", Org: org, InfrastructureProviderID: provider.ID, Status: cdbm.SiteStatusRegistered}
	_, err := dbSession.DB.NewInsert().Model(filteredSite).Exec(ctx)
	require.NoError(t, err)

	otherProvider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "other-provider", Org: "other-org"}
	_, err = dbSession.DB.NewInsert().Model(otherProvider).Exec(ctx)
	require.NoError(t, err)
	otherSite := &cdbm.Site{ID: uuid.New(), Name: "other-site", Org: "other-org", InfrastructureProviderID: otherProvider.ID, Status: cdbm.SiteStatusRegistered}
	_, err = dbSession.DB.NewInsert().Model(otherSite).Exec(ctx)
	require.NoError(t, err)
	emptyProviderOrg := "label-values-empty-provider-org"
	emptyProvider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "empty-provider", Org: emptyProviderOrg}
	_, err = dbSession.DB.NewInsert().Model(emptyProvider).Exec(ctx)
	require.NoError(t, err)

	emDAO := cdbm.NewExpectedMachineDAO(dbSession)
	for i, input := range []struct {
		siteID uuid.UUID
		value  string
	}{
		{site.ID, "fd-b"}, {site.ID, "fd-a"}, {site.ID, "fd-b"}, {filteredSite.ID, "fd-c"}, {otherSite.ID, "hidden"},
	} {
		_, err = emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID: uuid.New(), SiteID: input.siteID,
			BmcMacAddress: fmt.Sprintf("02:00:00:00:00:%02x", i), ChassisSerialNumber: uuid.NewString(),
			Labels: map[string]string{"failure-domain": input.value, "nvidia.com/failure-domain": input.value, "power-domain": "pd-1"}, CreatedBy: uuid.New(),
		})
		require.NoErrorf(t, err, "create ExpectedMachine %d", i)
	}

	machineDAO := cdbm.NewMachineDAO(dbSession)
	for i, input := range []struct {
		providerID uuid.UUID
		siteID     uuid.UUID
		value      string
	}{
		{provider.ID, site.ID, "fd-b"}, {provider.ID, site.ID, "fd-a"}, {provider.ID, site.ID, "fd-b"}, {provider.ID, filteredSite.ID, "fd-c"}, {otherProvider.ID, otherSite.ID, "hidden"},
	} {
		machineID := uuid.NewString()
		_, err = machineDAO.Create(ctx, nil, cdbm.MachineCreateInput{
			MachineID: machineID, InfrastructureProviderID: input.providerID, SiteID: input.siteID,
			ControllerMachineID: machineID, Status: cdbm.MachineStatusReady,
			Labels: map[string]string{"failure-domain": input.value, "nvidia.com/failure-domain": input.value, "power-domain": "pd-1"},
		})
		require.NoErrorf(t, err, "create Machine %d", i)
	}

	user := &cdbm.User{
		StarfleetID: cutil.GetPtr("label-values-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{
			ID: 1, Name: org, DisplayName: org, OrgType: "ENTERPRISE", Roles: []string{authz.ProviderViewerRole},
		}},
	}
	emptyProviderUser := &cdbm.User{
		StarfleetID: cutil.GetPtr("label-values-empty-provider-user"),
		OrgData: cdbm.OrgData{emptyProviderOrg: cdbm.Org{
			ID: 4, Name: emptyProviderOrg, DisplayName: emptyProviderOrg, OrgType: "ENTERPRISE", Roles: []string{authz.ProviderViewerRole},
		}},
	}
	nonMemberUser := &cdbm.User{StarfleetID: cutil.GetPtr("label-values-non-member-user"), OrgData: cdbm.OrgData{}}
	dualTenant := &cdbm.Tenant{ID: uuid.New(), Name: "label-values-dual-role-tenant", Org: org, CreatedBy: uuid.New()}
	_, err = dbSession.DB.NewInsert().Model(dualTenant).Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(&cdbm.TenantSite{
		ID: uuid.New(), TenantID: dualTenant.ID, TenantOrg: org, SiteID: otherSite.ID, CreatedBy: uuid.New(),
	}).Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(&cdbm.TenantAccount{
		ID: uuid.New(), AccountNumber: common.GenerateAccountNumber(), TenantID: &dualTenant.ID, TenantOrg: org,
		InfrastructureProviderID: otherProvider.ID, InfrastructureProviderOrg: otherProvider.Org,
		Status: cdbm.TenantAccountStatusReady, Config: cdbm.TenantAccountConfig{TargetedInstanceCreation: true}, CreatedBy: uuid.New(),
	}).Exec(ctx)
	require.NoError(t, err)
	dualRoleUser := &cdbm.User{
		StarfleetID: cutil.GetPtr("label-values-dual-role-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{
			ID: 5, Name: org, DisplayName: org, OrgType: "ENTERPRISE", Roles: []string{authz.ProviderViewerRole, authz.TenantAdminRole},
		}},
	}
	tenantOrg := "label-values-tenant-org"
	tenant := &cdbm.Tenant{ID: uuid.New(), Name: "label-values-tenant", Org: tenantOrg, CreatedBy: uuid.New()}
	_, err = dbSession.DB.NewInsert().Model(tenant).Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(&cdbm.TenantSite{
		ID: uuid.New(), TenantID: tenant.ID, TenantOrg: tenantOrg, SiteID: site.ID, CreatedBy: uuid.New(),
	}).Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(&cdbm.TenantAccount{
		ID: uuid.New(), AccountNumber: common.GenerateAccountNumber(), TenantID: &tenant.ID, TenantOrg: tenantOrg,
		InfrastructureProviderID: provider.ID, InfrastructureProviderOrg: org,
		Status: cdbm.TenantAccountStatusReady, Config: cdbm.TenantAccountConfig{TargetedInstanceCreation: true}, CreatedBy: uuid.New(),
	}).Exec(ctx)
	require.NoError(t, err)
	tenantUser := &cdbm.User{
		StarfleetID: cutil.GetPtr("label-values-tenant-user"),
		OrgData: cdbm.OrgData{tenantOrg: cdbm.Org{
			ID: 2, Name: tenantOrg, DisplayName: tenantOrg, OrgType: "ENTERPRISE", Roles: []string{authz.TenantAdminRole},
		}},
	}
	disabledTenantOrg := "label-values-disabled-tenant-org"
	disabledTenant := &cdbm.Tenant{ID: uuid.New(), Name: "label-values-disabled-tenant", Org: disabledTenantOrg, CreatedBy: uuid.New()}
	_, err = dbSession.DB.NewInsert().Model(disabledTenant).Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(&cdbm.TenantSite{
		ID: uuid.New(), TenantID: disabledTenant.ID, TenantOrg: disabledTenantOrg, SiteID: site.ID, CreatedBy: uuid.New(),
	}).Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(&cdbm.TenantAccount{
		ID: uuid.New(), AccountNumber: common.GenerateAccountNumber(), TenantID: &disabledTenant.ID, TenantOrg: disabledTenantOrg,
		InfrastructureProviderID: provider.ID, InfrastructureProviderOrg: org,
		Status: cdbm.TenantAccountStatusReady, Config: cdbm.TenantAccountConfig{TargetedInstanceCreation: false}, CreatedBy: uuid.New(),
	}).Exec(ctx)
	require.NoError(t, err)
	disabledTenantUser := &cdbm.User{
		StarfleetID: cutil.GetPtr("label-values-disabled-tenant-user"),
		OrgData: cdbm.OrgData{disabledTenantOrg: cdbm.Org{
			ID: 3, Name: disabledTenantOrg, DisplayName: disabledTenantOrg, OrgType: "ENTERPRISE", Roles: []string{authz.TenantAdminRole},
		}},
	}
	tests := []struct {
		name       string
		path       string
		routePath  string
		query      string
		key        string
		org        string
		user       *cdbm.User
		handle     func(echo.Context) error
		statusCode int
		wantItems  []string
		wantPage   *pagination.PageResponse
	}{
		{name: "Provider ExpectedMachine keys", path: "/expected-machine/label/key", org: org, user: user, handle: NewGetAllExpectedMachineLabelKeyHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"failure-domain", "nvidia.com/failure-domain", "power-domain"}, wantPage: pagination.NewPageResponse(1, 20, 3, cutil.GetPtr(labelKeyOrderByDefault))},
		{name: "Provider Machine keys", path: "/machine/label/key", org: org, user: user, handle: NewGetAllMachineLabelKeyHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"failure-domain", "nvidia.com/failure-domain", "power-domain"}},
		{name: "Provider ExpectedMachine values", path: "/expected-machine/label/key/failure-domain/value", key: "failure-domain", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c"}, wantPage: pagination.NewPageResponse(1, 20, 3, cutil.GetPtr(labelValueOrderByDefault))},
		{name: "Provider Machine values", path: "/machine/label/key/failure-domain/value", key: "failure-domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c"}},
		{name: "percent-encoded slash reaches handler through Echo router", path: "/expected-machine/label/key/nvidia.com%2Ffailure-domain/value", routePath: "/v2/org/:orgName/nico/expected-machine/label/key/:key/value", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c"}},
		{name: "Machine keys second page", path: "/machine/label/key", query: "?pageNumber=2&pageSize=1&orderBy=KEY_ASC", org: org, user: user, handle: NewGetAllMachineLabelKeyHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"nvidia.com/failure-domain"}, wantPage: pagination.NewPageResponse(2, 1, 3, cutil.GetPtr(labelKeyOrderByDefault))},
		{name: "Machine values second page", path: "/machine/label/key/failure-domain/value", query: "?pageNumber=2&pageSize=1&orderBy=VALUE_ASC", key: "failure-domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-b"}, wantPage: pagination.NewPageResponse(2, 1, 3, cutil.GetPtr(labelValueOrderByDefault))},
		{name: "Provider ExpectedMachine values for Site", path: "/expected-machine/label/key/failure-domain/value", query: "?siteId=" + site.ID.String(), key: "failure-domain", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b"}},
		{name: "Provider Machine values for Site", path: "/machine/label/key/failure-domain/value", query: "?siteId=" + site.ID.String(), key: "failure-domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b"}},
		{name: "Provider cannot scope ExpectedMachine values to another Provider Site", path: "/expected-machine/label/key/failure-domain/value", query: "?siteId=" + otherSite.ID.String(), key: "failure-domain", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusForbidden},
		{name: "Provider cannot scope Machine values to another Provider Site", path: "/machine/label/key/failure-domain/value", query: "?siteId=" + otherSite.ID.String(), key: "failure-domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusForbidden},
		{name: "Provider with no Sites gets no ExpectedMachine keys", path: "/expected-machine/label/key", org: emptyProviderOrg, user: emptyProviderUser, handle: NewGetAllExpectedMachineLabelKeyHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{}, wantPage: pagination.NewPageResponse(1, 20, 0, cutil.GetPtr(labelKeyOrderByDefault))},
		{name: "unknown ExpectedMachine label key returns empty values", path: "/expected-machine/label/key/absent/value", key: "absent", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{}},
		{name: "unknown Machine label key returns empty values", path: "/machine/label/key/absent/value", key: "absent", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{}},
		{name: "dual-role org ExpectedMachine values include Provider and privileged Tenant Sites", path: "/expected-machine/label/key/failure-domain/value", key: "failure-domain", org: org, user: dualRoleUser, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c", "hidden"}},
		{name: "dual-role org Machine values mirror Provider-scoped Machine list", path: "/machine/label/key/failure-domain/value", key: "failure-domain", org: org, user: dualRoleUser, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c"}},
		{name: "privileged Tenant ExpectedMachine values", path: "/expected-machine/label/key/failure-domain/value", key: "failure-domain", org: tenantOrg, user: tenantUser, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c"}},
		{name: "privileged Tenant Machine values", path: "/machine/label/key/failure-domain/value", key: "failure-domain", org: tenantOrg, user: tenantUser, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusOK, wantItems: []string{"fd-a", "fd-b", "fd-c"}},
		{name: "unprivileged Tenant cannot get ExpectedMachine values", path: "/expected-machine/label/key/failure-domain/value", key: "failure-domain", org: disabledTenantOrg, user: disabledTenantUser, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusForbidden},
		{name: "unprivileged Tenant cannot get Machine values", path: "/machine/label/key/failure-domain/value", key: "failure-domain", org: disabledTenantOrg, user: disabledTenantUser, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusForbidden},
		{name: "ExpectedMachine unknown query parameter", path: "/expected-machine/label/key/failure-domain/value", query: "?siteID=" + site.ID.String(), key: "failure-domain", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusBadRequest},
		{name: "Machine unknown query parameter", path: "/machine/label/key/failure-domain/value", query: "?siteID=" + site.ID.String(), key: "failure-domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusBadRequest},
		{name: "keys reject value order", path: "/machine/label/key", query: "?orderBy=VALUE_ASC", org: org, user: user, handle: NewGetAllMachineLabelKeyHandler(dbSession).Handle, statusCode: http.StatusBadRequest},
		{name: "values reject key order", path: "/machine/label/key/failure-domain/value", query: "?orderBy=KEY_ASC", key: "failure-domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusBadRequest},
		{name: "invalid ExpectedMachine label key", path: "/expected-machine/label/key/invalid/value", key: "   ", org: org, user: user, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusBadRequest},
		{name: "NUL Machine label key", path: "/machine/label/key/invalid/value", key: "failure\x00domain", org: org, user: user, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusBadRequest},
		{name: "non-member cannot validate ExpectedMachine label key", path: "/expected-machine/label/key/invalid/value", key: "   ", org: org, user: nonMemberUser, handle: NewGetAllExpectedMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusForbidden},
		{name: "non-member cannot validate Machine label key", path: "/machine/label/key/invalid/value", key: "   ", org: org, user: nonMemberUser, handle: NewGetAllMachineLabelValueHandler(dbSession).Handle, statusCode: http.StatusForbidden},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/v2/org/"+tt.org+"/nico"+tt.path+tt.query, nil)
			rec := httptest.NewRecorder()
			if tt.routePath != "" {
				e.Use(func(next echo.HandlerFunc) echo.HandlerFunc {
					return func(c echo.Context) error {
						c.Set("user", tt.user)
						return next(c)
					}
				})
				e.GET(tt.routePath, tt.handle)
				e.ServeHTTP(rec, req)
			} else {
				ec := e.NewContext(req, rec)
				ec.Set("user", tt.user)
				ec.SetParamNames("orgName", "key")
				ec.SetParamValues(tt.org, tt.key)
				require.NoError(t, tt.handle(ec))
			}
			assert.Equal(t, tt.statusCode, rec.Code)
			if tt.statusCode == http.StatusOK {
				var items []string
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &items))
				assert.Equal(t, tt.wantItems, items)
				pageHeader := rec.Header().Get(pagination.ResponseHeaderName)
				require.NotEmpty(t, pageHeader)
				if tt.wantPage != nil {
					pageResponse := pagination.PageResponse{}
					require.NoError(t, json.Unmarshal([]byte(pageHeader), &pageResponse))
					assert.Equal(t, *tt.wantPage, pageResponse)
				}
			}
		})
	}
}

func TestGetLabelKey(t *testing.T) {
	tests := []struct {
		name    string
		param   string
		want    string
		wantErr bool
	}{
		{name: "plain key", param: "failure-domain", want: "failure-domain"},
		{name: "percent-encoded slash", param: "nvidia.com%2Ffailure-domain", want: "nvidia.com/failure-domain"},
		{name: "malformed escape", param: "%ZZ", wantErr: true},
		{name: "empty key", param: "", wantErr: true},
		{name: "whitespace key", param: "   ", wantErr: true},
		{name: "over-length key", param: strings.Repeat("k", modelutil.LabelKeyMaxLength+1), wantErr: true},
		{name: "NUL key", param: "failure\x00domain", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("key")
			ec.SetParamValues(tt.param)
			got, gotErr := getLabelKey(ec)
			assert.Equal(t, tt.wantErr, gotErr != nil)
			assert.Equal(t, tt.want, got)
		})
	}
}
