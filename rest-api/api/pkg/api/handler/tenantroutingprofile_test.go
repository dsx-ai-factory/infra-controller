// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestUpdateTenantRoutingProfileHandler_Handle(t *testing.T) {
	cases := []struct {
		name          string
		currentTenant bool
		rejectProfile bool
		prepare       func(*testing.T, *tenantRoutingProfileFixture)
		status        int
		calls         int
	}{
		{
			name:          "tenant cannot lower its default access tier",
			currentTenant: true,
			rejectProfile: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitions.Profiles[1].Config.AccessTier = cutil.GetPtr(uint32(1))
			},
			status: http.StatusForbidden,
			calls:  2,
		},
		{
			name:          "tenant may raise its default access tier",
			currentTenant: true,
			rejectProfile: false,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitions.Profiles[1].Config.AccessTier = cutil.GetPtr(uint32(3))
			},
			status: http.StatusOK,
			calls:  2,
		},
		{
			name:          "unset destination access tier counts as zero",
			currentTenant: true,
			rejectProfile: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitions.Profiles[1].Config.AccessTier = nil
			},
			status: http.StatusForbidden,
			calls:  2,
		},
		{
			name:          "unset default access tier counts as zero",
			currentTenant: true,
			rejectProfile: false,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitions.Profiles[0].Config.AccessTier = nil
				f.definitions.Profiles[1].Config.AccessTier = nil
			},
			status: http.StatusOK,
			calls:  2,
		},
		{
			name:          "missing current default fails closed",
			currentTenant: true,
			rejectProfile: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitions.Profiles = f.definitions.Profiles[1:]
			},
			status: http.StatusInternalServerError,
			calls:  2,
		},
		{
			name:          "missing requested profile is invalid",
			currentTenant: true,
			rejectProfile: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitions.Profiles = f.definitions.Profiles[:1]
			},
			status: http.StatusBadRequest,
			calls:  2,
		},
		{
			name:          "definition lookup error prevents mutation",
			currentTenant: true,
			rejectProfile: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.definitionsErr = temporal.NewNonRetryableApplicationError("Core unavailable", swe.ErrTypeNICoUnavailable, nil)
			},
			status: http.StatusServiceUnavailable,
			calls:  2,
		},
		{
			name:          "privileged tenant updates its own default",
			currentTenant: true,
			status:        http.StatusOK,
			calls:         2,
		},
		{
			name:          "tenant cannot select another organization in the body or query",
			currentTenant: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.body = fmt.Sprintf(`{"siteId":%q,"defaultRoutingProfile":"internal","tenantId":%q,"organizationId":"other-tenant"}`, f.siteID, uuid.NewString())
			},
			status: http.StatusOK,
			calls:  2,
		},
		{
			name:          "unprivileged tenant cannot update its default",
			currentTenant: true,
			prepare:       func(t *testing.T, f *tenantRoutingProfileFixture) { f.setPrivilege(t, false) },
			status:        http.StatusForbidden,
		},
		{
			name:          "site denial overrides account privilege",
			currentTenant: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				cdbm.TestBuildTenantSite(t, f.db, f.tenant, f.site, &cdbm.TenantSiteConfig{
					TargetedInstanceCreation: cutil.GetPtr(false),
				}, f.user)
			},
			status: http.StatusForbidden,
		},
		{
			name:          "site grant permits update without account-wide privilege",
			currentTenant: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.setPrivilege(t, false)
				cdbm.TestBuildTenantSite(t, f.db, f.tenant, f.site, &cdbm.TenantSiteConfig{
					TargetedInstanceCreation: cutil.GetPtr(true),
				}, f.user)
			},
			status: http.StatusOK,
			calls:  2,
		},
		{
			name:          "privilege at another provider does not grant access",
			currentTenant: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				other := common.TestBuildInfrastructureProvider(t, f.db, "Other", "other-provider", f.user)
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("infrastructure_provider_id = ?", other.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name:          "site grant still requires a Ready account",
			currentTenant: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				cdbm.TestBuildTenantSite(t, f.db, f.tenant, f.site, &cdbm.TenantSiteConfig{
					TargetedInstanceCreation: cutil.GetPtr(true),
				}, f.user)
				_, err := f.db.DB.NewUpdate().Model(f.account).Set("status = ?", cdbm.TenantAccountStatusInvited).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name:          "provider role alone cannot use current tenant endpoint",
			currentTenant: true,
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, []string{authz.ProviderAdminRole})
			},
			status: http.StatusForbidden,
		},
		{
			name:          "tenant cannot use another org path",
			currentTenant: true,
			prepare:       func(t *testing.T, f *tenantRoutingProfileFixture) { f.org = "other-tenant" },
			status:        http.StatusForbidden,
		},
		{
			name:   "provider changes default with metadata and version preserved",
			status: http.StatusOK,
			calls:  2,
		},
		{
			name: "configured profile is passed unchanged",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.profile = "TENANT_EDGE"
				f.updated.Tenant.RoutingProfileType = cutil.GetPtr(f.profile)
			},
			status: http.StatusOK,
			calls:  2,
		},
		{
			name:    "missing user",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.user = nil },
			status:  http.StatusInternalServerError,
		},
		{
			name:    "wrong org membership",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.org = "another-org" },
			status:  http.StatusForbidden,
		},
		{
			name: "provider viewer cannot mutate",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, []string{authz.ProviderViewerRole})
			},
			status: http.StatusForbidden,
		},
		{
			name: "privileged tenant cannot use provider route",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.org = f.account.TenantOrg
				f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, []string{authz.TenantAdminRole})
				_, err := cdbm.NewTenantAccountDAO(f.db).Update(context.Background(), nil, cdbm.TenantAccountUpdateInput{
					TenantAccountID: f.account.ID,
					Config: &cdbm.TenantAccountConfig{
						TargetedInstanceCreation: true,
					},
				})
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name:    "malformed tenant ID",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.tenantID = "invalid" },
			status:  http.StatusBadRequest,
		},
		{
			name:    "missing tenant",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.tenantID = uuid.NewString() },
			status:  http.StatusNotFound,
		},
		{
			name:    "account ID cannot be used as Tenant ID",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.tenantID = f.account.ID.String() },
			status:  http.StatusNotFound,
		},
		{
			name: "unlinked Tenant cannot be updated by Provider",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				_, err := f.db.DB.NewUpdate().Model(f.account).Set("tenant_id = NULL").WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name: "foreign account",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				other := common.TestBuildInfrastructureProvider(t, f.db, "Other", "other-provider", f.user)
				_, err := f.db.DB.NewUpdate().Model(f.account).Set("infrastructure_provider_id = ?", other.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name: "unaccepted account",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				_, err := f.db.DB.NewUpdate().Model(f.account).Set("status = ?", cdbm.TenantAccountStatusInvited).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusBadRequest,
		},
		{
			name:    "malformed JSON",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.body = "{" },
			status:  http.StatusBadRequest,
		},
		{
			name:    "invalid request rejected before proxy",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.profile = "" },
			status:  http.StatusBadRequest,
		},
		{
			name:    "missing site",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.siteID = uuid.NewString() },
			status:  http.StatusBadRequest,
		},
		{
			name: "foreign site",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				other := common.TestBuildInfrastructureProvider(t, f.db, "Other", "other-provider", f.user)
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("infrastructure_provider_id = ?", other.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name: "unregistered site",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("status = ?", cdbm.SiteStatusPending).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusBadRequest,
		},
		{
			name: "site without native networking",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("config = NULL").WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusBadRequest,
		},
		{
			name:    "tenant missing on Core",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.current.Tenant = nil },
			status:  http.StatusNotFound,
			calls:   1,
		},
		{
			name: "mismatched Core identity cannot target another tenant",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.current.Tenant.OrganizationId = "other-tenant"
			},
			status: http.StatusInternalServerError,
			calls:  1,
		},
		{
			name:    "missing concurrency token prevents update",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.current.Tenant.Version = "" },
			status:  http.StatusInternalServerError,
			calls:   1,
		},
		{
			name:    "metadata cannot be reconstructed from stale REST state",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.current.Tenant.Metadata = nil },
			status:  http.StatusInternalServerError,
			calls:   1,
		},
		{
			name: "Core read failure prevents mutation",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.readErr = temporal.NewNonRetryableApplicationError("Core unavailable", swe.ErrTypeNICoUnavailable, nil)
			},
			status: http.StatusServiceUnavailable,
			calls:  1,
		},
		{
			name: "active FNN VPC rejection is preserved",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.updateErr = temporal.NewNonRetryableApplicationError("tenant has active FNN VPCs", swe.ErrTypeNICoFailedPrecondition, nil)
			},
			status: http.StatusPreconditionFailed,
			calls:  2,
		},
		{
			name: "concurrent update rejection is preserved without retry",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.updateErr = temporal.NewNonRetryableApplicationError("tenant version changed", swe.ErrTypeNICoFailedPrecondition, nil)
			},
			status: http.StatusPreconditionFailed,
			calls:  2,
		},
		{
			name: "unknown profile rejected by Core",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.updateErr = temporal.NewNonRetryableApplicationError("RoutingProfile not found", swe.ErrTypeNICoObjectNotFound, nil)
			},
			status: http.StatusBadRequest,
			calls:  2,
		},
		{
			name:    "empty mutation response is not success",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) { f.updated.Tenant = nil },
			status:  http.StatusInternalServerError,
			calls:   2,
		},
		{
			name: "unchanged response is not success",
			prepare: func(t *testing.T, f *tenantRoutingProfileFixture) {
				f.updated.Tenant.RoutingProfileType = cutil.GetPtr("EXTERNAL")
			},
			status: http.StatusInternalServerError,
			calls:  2,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newTenantRoutingProfileFixture(t)
			if tc.currentTenant {
				f.org = f.tenant.Org
				f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, []string{authz.TenantAdminRole})
				f.tenantID = ""
				f.setPrivilege(t, true)
			}
			if tc.prepare != nil {
				tc.prepare(t, f)
			}
			if f.body == "" {
				f.body = fmt.Sprintf(`{"siteId":%q,"defaultRoutingProfile":%q}`, f.siteID, f.profile)
			}
			if tc.calls > 0 {
				f.expectProxy(t, corev1.Forge_FindTenant_FullMethodName, f.current, f.readErr)
			}
			expectedCalls := tc.calls
			if tc.currentTenant && tc.calls > 1 {
				f.expectProxy(t, corev1.Forge_FindRoutingProfilesByNames_FullMethodName, f.definitions, f.definitionsErr)
				if !tc.rejectProfile {
					expectedCalls++
				}
			}
			if tc.calls > 1 && !tc.rejectProfile {
				f.expectProxy(t, corev1.Forge_UpdateTenant_FullMethodName, f.updated, f.updateErr)
			}
			e := echo.New()
			h := NewUpdateTenantRoutingProfileHandler(f.db, f.scp)
			path := "/org/:orgName/nico/tenant/:id/routing-profile"
			url := "/org/" + f.org + "/nico/tenant/" + f.tenantID + "/routing-profile"
			if tc.currentTenant {
				path = "/org/:orgName/nico/tenant/current/routing-profile"
				url = "/org/" + f.org + "/nico/tenant/current/routing-profile?tenantId=" + uuid.NewString()
			}
			e.PATCH(path, h.Handle, func(next echo.HandlerFunc) echo.HandlerFunc {
				return func(c echo.Context) error {
					c.Set("user", f.user)
					return next(c)
				}
			})
			req := httptest.NewRequest(http.MethodPatch, url, strings.NewReader(f.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			e.ServeHTTP(rec, req)
			assert.Equal(t, tc.status, rec.Code, rec.Body.String())
			require.Len(t, f.calls, expectedCalls)
			f.client.AssertExpectations(t)
			if tc.calls > 0 {
				var read corev1.FindTenantRequest
				require.NoError(t, protojson.Unmarshal(f.calls[0].RequestJSON, &read))
				assert.Equal(t, f.tenant.Org, read.TenantOrganizationId)
			}
			if tc.currentTenant && tc.calls > 1 {
				var lookup corev1.RoutingProfilesByNamesRequest
				require.NoError(t, protojson.Unmarshal(f.calls[1].RequestJSON, &lookup))
				assert.Equal(t, []string{"EXTERNAL", "INTERNAL"}, lookup.Names)
			}
			if tc.calls > 1 && !tc.rejectProfile {
				var update corev1.UpdateTenantRequest
				require.NoError(t, protojson.Unmarshal(f.calls[len(f.calls)-1].RequestJSON, &update))
				assert.Equal(t, f.tenant.Org, update.OrganizationId)
				assert.Equal(t, f.current.Tenant.Version, update.GetIfVersionMatch())
				assert.True(t, proto.Equal(f.current.Tenant.Metadata, update.Metadata))
				assert.Empty(t, f.calls[len(f.calls)-1].EncryptedSecrets)
			}
			if tc.status == http.StatusOK {
				assert.JSONEq(t, fmt.Sprintf(`{"siteId":%q,"defaultRoutingProfile":%q}`, f.siteID, f.profile), rec.Body.String())
			}
		})
	}
}

type tenantRoutingProfileFixture struct {
	db             *cdb.Session
	org            string
	user           *cdbm.User
	site           *cdbm.Site
	account        *cdbm.TenantAccount
	tenant         *cdbm.Tenant
	tenantID       string
	siteID         string
	profile        string
	body           string
	current        *corev1.FindTenantResponse
	updated        *corev1.UpdateTenantResponse
	readErr        error
	updateErr      error
	definitions    *corev1.NamedRoutingProfileList
	definitionsErr error
	client         *tmocks.Client
	scp            *sc.ClientPool
	calls          []grpcproxy.Request
}

func newTenantRoutingProfileFixture(t *testing.T) *tenantRoutingProfileFixture {
	t.Helper()
	db := common.TestInitDB(t)
	t.Cleanup(db.Close)
	common.TestSetupSchema(t, db)
	org := "provider-org"
	user := common.TestBuildUser(t, db, uuid.NewString(), org, []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, db, "Provider", org, user)
	tenant := common.TestBuildTenant(t, db, "REST display name", "tenant-org", user)
	account := common.TestBuildTenantAccount(t, db, provider, &tenant.ID, tenant.Org, cdbm.TenantAccountStatusReady, user)
	site := common.TestBuildSite(t, db, provider, "Site", user)
	_, err := cdbm.NewSiteDAO(db).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
		Config: &cdbm.SiteConfigUpdateInput{
			NativeNetworking: cutil.GetPtr(true),
		},
	})
	require.NoError(t, err)
	client := &tmocks.Client{}
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = client
	current := &corev1.Tenant{
		OrganizationId: tenant.Org,
		Metadata: &corev1.Metadata{
			Name: "Core's authoritative display name",
		},
		Version:            "V7-T1761856992374052",
		RoutingProfileType: cutil.GetPtr("EXTERNAL"),
	}
	updated := proto.Clone(current).(*corev1.Tenant)
	updated.Version = "V8-T1761856992374053"
	updated.RoutingProfileType = cutil.GetPtr("INTERNAL")
	return &tenantRoutingProfileFixture{
		db:       db,
		org:      org,
		user:     user,
		site:     site,
		account:  account,
		tenant:   tenant,
		tenantID: tenant.ID.String(),
		siteID:   site.ID.String(),
		profile:  "internal",
		current: &corev1.FindTenantResponse{
			Tenant: current,
		},
		updated: &corev1.UpdateTenantResponse{
			Tenant: updated,
		},
		definitions: &corev1.NamedRoutingProfileList{
			Profiles: []*corev1.NamedRoutingProfile{
				{
					Name:   "EXTERNAL",
					Config: &corev1.NamedRoutingProfileConfig{AccessTier: cutil.GetPtr(uint32(2))},
				},
				{
					Name:   "INTERNAL",
					Config: &corev1.NamedRoutingProfileConfig{AccessTier: cutil.GetPtr(uint32(2))},
				},
			},
		},
		client: client,
		scp:    scp,
	}
}

func (f *tenantRoutingProfileFixture) expectProxy(t *testing.T, method string, response proto.Message, resultErr error) {
	t.Helper()
	run := &tmocks.WorkflowRun{}
	run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		out := args.Get(1).(*grpcproxy.Response)
		data, err := protojson.Marshal(response)
		require.NoError(t, err)
		out.ResponseJSON = data
	}).Return(resultErr).Once()
	f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool { return req.FullMethod == method }),
	).Run(func(args mock.Arguments) {
		f.calls = append(f.calls, args.Get(3).(grpcproxy.Request))
	}).Return(run, nil).Once()
	t.Cleanup(func() { run.AssertExpectations(t) })
}

func (f *tenantRoutingProfileFixture) setPrivilege(t *testing.T, enabled bool) {
	t.Helper()
	_, err := cdbm.NewTenantAccountDAO(f.db).Update(context.Background(), nil, cdbm.TenantAccountUpdateInput{
		TenantAccountID: f.account.ID,
		Config: &cdbm.TenantAccountConfig{
			TargetedInstanceCreation: enabled,
		},
	})
	require.NoError(t, err)
}
