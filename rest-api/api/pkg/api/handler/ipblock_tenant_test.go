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
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/middleware"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sdk "github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	inventory "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/siteprefix"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/require"
	tclient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"
)

type tenantIPBlockClient struct {
	tclient.Client
	call func(context.Context, grpcproxy.Request) (tclient.WorkflowRun, error)
}

func (c tenantIPBlockClient) ExecuteWorkflow(ctx context.Context, _ tclient.StartWorkflowOptions, _ any, args ...any) (tclient.WorkflowRun, error) {
	return c.call(ctx, args[0].(grpcproxy.Request))
}

type tenantIPBlockRun struct {
	tclient.WorkflowRun
	data []byte
	err  error
	wait bool
}

func (r tenantIPBlockRun) Get(ctx context.Context, value any) error {
	if r.wait {
		<-ctx.Done()
		return ctx.Err()
	}
	if value != nil {
		value.(*grpcproxy.Response).ResponseJSON = r.data
	}
	return r.err
}

type tenantIPBlockFixture struct {
	session            *cdb.Session
	site               *cdbm.Site
	tenant             *cdbm.Tenant
	user, providerUser *cdbm.User
	router             *echo.Echo
	core               *corev1.SitePrefix
	calls              []string
	nextError          error
	commitOnError      bool
	lastCreatedID      uuid.UUID
	beforeUpdate       func()
	nextWait           bool
	requestTimeout     time.Duration
	failMethod         string
	deadlines          []time.Time
	org                string
	duringCore         func()
	siteRequestContext context.Context
}

func newTenantIPBlockFixture(t *testing.T) *tenantIPBlockFixture {
	t.Helper()
	f := &tenantIPBlockFixture{session: common.TestInitDB(t), org: "tenant"}
	t.Cleanup(f.session.Close)
	common.TestSetupSchema(t, f.session)
	f.user = common.TestBuildUser(t, f.session, uuid.NewString(), "tenant", []string{auth.TenantAdminRole})
	f.providerUser = common.TestBuildUser(t, f.session, uuid.NewString(), "provider", []string{auth.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, f.session, "provider", "provider", f.providerUser)
	f.site = common.TestBuildSite(t, f.session, provider, "site", f.providerUser)
	var err error
	f.site, err = cdbm.NewSiteDAO(f.session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered), Config: &cdbm.SiteConfigUpdateInput{TenantSitePrefix: cutil.GetPtr(true)}})
	require.NoError(t, err)
	f.tenant = common.TestBuildTenant(t, f.session, "tenant", "tenant", f.user)
	common.TestBuildTenantSite(t, f.session, f.tenant, f.site, f.user)
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[f.site.ID.String()] = tenantIPBlockClient{call: func(ctx context.Context, request grpcproxy.Request) (tclient.WorkflowRun, error) {
		deadline, ok := ctx.Deadline()
		require.True(t, ok)
		f.deadlines = append(f.deadlines, deadline)
		f.calls = append(f.calls, request.FullMethod)
		result := tenantIPBlockRun{wait: f.nextWait}
		commitOnError := false
		f.nextWait = false
		if f.failMethod == "" || request.FullMethod == f.failMethod {
			result.err = f.nextError
			f.nextError = nil
			commitOnError = f.commitOnError
			f.commitOnError = false
		}
		// Definitive failures reject before mutation; uncertain outcomes may commit.
		apply := result.err == nil || commitOnError
		var data []byte
		var err error
		switch request.FullMethod {
		case createTenantIPBlockMethod:
			req := &corev1.SitePrefixCreationRequest{}
			require.NoError(t, protojson.Unmarshal(request.RequestJSON, req))
			require.Equal(t, f.tenant.Org, req.TenantOrganizationId)
			// The row is in the caller's transaction and must not be committed yet.
			blocks, _, findErr := cdbm.NewIPBlockDAO(f.session).GetAll(context.Background(), nil, cdbm.IPBlockFilterInput{SitePrefixIDs: []uuid.UUID{uuid.MustParse(req.Id.Value)}}, cdbp.PageInput{}, nil)
			require.NoError(t, findErr)
			require.Empty(t, blocks)
			f.lastCreatedID = uuid.MustParse(req.GetId().GetValue())
			if !apply {
				return result, nil
			}
			f.core = &corev1.SitePrefix{Id: req.Id, Version: "v1", Metadata: req.Metadata, Config: &corev1.SitePrefixConfig{Prefix: req.Prefix, TenantOrganizationId: &req.TenantOrganizationId, RoutingScope: corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY}, Status: &corev1.SitePrefixStatus{Authority: corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED, LifecycleState: corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY}}
			data, err = protojson.Marshal(f.core)
		case findTenantIPBlockMethod:
			req := &corev1.SitePrefixesByIdsRequest{}
			require.NoError(t, protojson.Unmarshal(request.RequestJSON, req))
			require.Len(t, req.SitePrefixIds, 1)
			require.Equal(t, f.core.Id.Value, req.SitePrefixIds[0].GetValue())
			data, err = protojson.Marshal(&corev1.SitePrefixList{SitePrefixes: []*corev1.SitePrefix{f.core}})
		case updateTenantIPBlockMethod:
			req := &corev1.SitePrefixUpdateRequest{}
			require.NoError(t, protojson.Unmarshal(request.RequestJSON, req))
			require.Equal(t, f.core.Id.Value, req.GetId().GetValue())
			require.Equal(t, f.tenant.Org, req.TenantOrganizationId)
			require.NotNil(t, req.IfVersionMatch)
			if f.beforeUpdate != nil {
				f.beforeUpdate()
				f.beforeUpdate = nil
			}
			if req.GetIfVersionMatch() != f.core.Version {
				return tenantIPBlockRun{err: tp.NewNonRetryableApplicationError("stale SitePrefix version", swe.ErrTypeNICoFailedPrecondition, nil)}, nil
			}
			require.NotNil(t, req.Metadata)
			if !apply {
				return result, nil
			}
			f.core.Metadata = req.Metadata
			f.core.Version = uuid.NewString()
			data, err = protojson.Marshal(f.core)
		case deleteTenantIPBlockMethod:
			req := &corev1.SitePrefixDeletionRequest{}
			require.NoError(t, protojson.Unmarshal(request.RequestJSON, req))
			require.Equal(t, f.core.Id.Value, req.GetId().GetValue())
			require.Equal(t, f.tenant.Org, req.TenantOrganizationId)
			if !apply {
				return result, nil
			}
			f.core.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			data, err = protojson.Marshal(&corev1.SitePrefixDeletionResult{SitePrefix: f.core})
		default:
			t.Fatalf("unexpected Core method %s", request.FullMethod)
		}
		require.NoError(t, err)
		result.data = data
		if f.duringCore != nil {
			f.duringCore()
		}
		return result, nil
	}}
	f.router = echo.New()
	f.router.Use(middleware.AuditBody(f.session), middleware.AuditLog(f.session))
	f.router.Use(func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			c.Set("user", f.user)
			if f.siteRequestContext != nil && strings.Contains(c.Request().URL.Path, "/site/") {
				c.SetRequest(c.Request().WithContext(f.siteRequestContext))
			}
			return next(c)
		}
	})
	base := "/v2/org/:orgName/nico/ipblock"
	cfg := common.GetTestConfig()
	f.router.POST(base, NewCreateIPBlockHandler(f.session, nil, pool, cfg).Handle)
	f.router.GET(base, NewGetAllIPBlockHandler(f.session, nil, cfg).Handle)
	f.router.GET(base+"/:id", NewGetIPBlockHandler(f.session, nil, cfg).Handle)
	f.router.PATCH(base+"/:id", NewUpdateIPBlockHandler(f.session, nil, pool, cfg).Handle)
	f.router.DELETE(base+"/:id", NewDeleteIPBlockHandler(f.session, nil, pool, cfg).Handle)
	f.router.PATCH("/v2/org/:orgName/nico/site/:id", NewUpdateSiteHandler(f.session, nil, cfg).Handle)
	f.router.DELETE("/v2/org/:orgName/nico/site/:id", NewDeleteSiteHandler(f.session, nil, cfg).Handle)
	f.router.DELETE("/v2/org/:orgName/nico/allocation/:id", NewDeleteAllocationHandler(f.session, nil, cfg).Handle)
	f.router.POST("/custom/org/:orgName/carbide/ipblock", NewCreateIPBlockHandler(f.session, nil, pool, cfg).Handle)

	return f
}
func (f *tenantIPBlockFixture) request(t *testing.T, method, path string, body any) *httptest.ResponseRecorder {
	t.Helper()
	data, err := json.Marshal(body)
	require.NoError(t, err)
	req := httptest.NewRequest(method, "/v2/org/"+f.org+"/nico/ipblock"+path, strings.NewReader(string(data)))
	if f.requestTimeout > 0 {
		ctx, cancel := context.WithTimeout(req.Context(), f.requestTimeout)
		defer cancel()
		req = req.WithContext(ctx)
	}
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	f.router.ServeHTTP(rec, req)
	return rec
}
func (f *tenantIPBlockFixture) body() model.APIIPBlockCreateRequest {
	return model.APIIPBlockCreateRequest{Name: "private-root", SiteID: f.site.ID.String(), Prefix: "10.21.0.0", PrefixLength: 24, ProtocolVersion: cdbm.IPBlockProtocolVersionV4, RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly}
}
func readTenantIPBlock(t *testing.T, r *httptest.ResponseRecorder, code int) model.APIIPBlock {
	t.Helper()
	require.Equal(t, code, r.Code, r.Body.String())
	var fields map[string]json.RawMessage
	require.NoError(t, json.Unmarshal(r.Body.Bytes(), &fields))
	require.Contains(t, fields, "managed")
	var result model.APIIPBlock
	require.NoError(t, json.Unmarshal(r.Body.Bytes(), &result))
	return result
}
func checkTenantIPBlockLifecycle(t *testing.T, method string) {
	t.Helper()
	for _, row := range []struct {
		// An empty method shares failure cases between PATCH and DELETE.
		method string
		name   string
		check  func(*testing.T, *tenantIPBlockFixture)
	}{
		{http.MethodPost, "Core latency does not block routine Site updates", func(t *testing.T, f *tenantIPBlockFixture) {
			f.duringCore = func() {
				ctx, cancel := context.WithTimeout(context.Background(), time.Second)
				defer cancel()
				updated, err := cdbm.NewSiteDAO(f.session).Update(ctx, nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, InventoryReceived: cutil.GetPtr(time.Now())})
				require.NoError(t, err)
				require.NotNil(t, updated.InventoryReceived)
				f.duringCore = nil
				original := f.core
				body := f.body()
				body.Name, body.Prefix = "independent", "10.25.0.0"
				other := readTenantIPBlock(t, f.request(t, http.MethodPost, "", body), http.StatusCreated)
				require.NotEqual(t, original.Id.Value, other.ID)
				f.core = original
			}
			readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
		}},
		{http.MethodPost, "capability writer prevents Core admission and is rechecked after commit", func(t *testing.T, f *tenantIPBlockFixture) {
			ctx := context.Background()
			tx, err := cdb.BeginTx(ctx, f.session, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			_, err = cdbm.NewSiteDAO(f.session).Update(ctx, tx, cdbm.SiteUpdateInput{SiteID: f.site.ID, Config: &cdbm.SiteConfigUpdateInput{TenantSitePrefix: cutil.GetPtr(false)}})
			require.NoError(t, err)
			require.Equal(t, http.StatusConflict, f.request(t, http.MethodPost, "", f.body()).Code)
			require.Empty(t, f.calls)
			require.NoError(t, tx.Commit())
			require.Equal(t, http.StatusPreconditionFailed, f.request(t, http.MethodPost, "", f.body()).Code)
			require.Empty(t, f.calls)
		}},
		{http.MethodPost, "inventory skips an in-flight prefix and reconciles the rest of the page", func(t *testing.T, f *tenantIPBlockFixture) {
			for _, method := range []string{http.MethodPost, http.MethodPatch, http.MethodDelete} {
				t.Run(method, func(t *testing.T) {
					path := ""
					var body any = f.body()
					want := http.StatusCreated
					if method != http.MethodPost {
						path = "/" + f.core.Id.Value
						body = map[string]any{"name": "updated"}
						want = http.StatusOK
						if method == http.MethodDelete {
							body = nil
							want = http.StatusAccepted
						}
					}
					receiver := inventory.NewManageSitePrefix(f.session)
					f.duringCore = func() {
						// The POST row is still invisible; PATCH/DELETE hold an existing row.
						other := proto.Clone(f.core).(*corev1.SitePrefix)
						other.Id = &corev1.SitePrefixId{Value: uuid.NewString()}
						other.Config.Prefix = map[string]string{http.MethodPost: "10.22.0.0/24", http.MethodPatch: "10.23.0.0/24", http.MethodDelete: "10.24.0.0/24"}[method]
						other.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY
						report := &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, Timestamp: timestamppb.Now(), SitePrefixes: []*corev1.SitePrefix{f.core, other}}
						ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
						defer cancel()
						require.NoError(t, receiver.UpdateSitePrefixesInDB(ctx, f.site.ID, report))
						got, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, uuid.MustParse(other.Id.Value), nil)
						require.NoError(t, err)
						require.Equal(t, cdbm.IPBlockStatusReady, got.Status)
						_, err = cdbm.NewSiteDAO(f.session).Update(ctx, nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, InventoryReceived: cutil.GetPtr(time.Now())})
						require.NoError(t, err)
						f.duringCore = nil
					}
					require.Equal(t, want, f.request(t, method, path, body).Code)
					report := &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, Timestamp: timestamppb.Now(), SitePrefixes: []*corev1.SitePrefix{f.core}}
					require.NoError(t, receiver.UpdateSitePrefixesInDB(context.Background(), f.site.ID, report))
					got, err := cdbm.NewIPBlockDAO(f.session).GetByID(context.Background(), nil, uuid.MustParse(f.core.Id.Value), nil)
					require.NoError(t, err)
					if method == http.MethodDelete {
						require.Equal(t, cdbm.IPBlockStatusDeleting, got.Status)
					} else {
						require.Equal(t, cdbm.IPBlockStatusReady, got.Status)
					}
				})
			}
		}},
		{http.MethodPost, "create metadata deletion and inventory", func(t *testing.T, f *tenantIPBlockFixture) {
			result := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			require.Equal(t, cdbm.IPBlockStatusProvisioning, result.Status)
			require.False(t, result.Managed)
			block, err := cdbm.NewIPBlockDAO(f.session).GetByID(context.Background(), nil, uuid.MustParse(result.ID), nil)
			require.NoError(t, err)
			require.False(t, block.Managed)
			require.Equal(t, block.ID, *block.SitePrefixID)
			report := &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, Timestamp: timestamppb.Now(), SitePrefixes: []*corev1.SitePrefix{f.core}}
			receiver := inventory.NewManageSitePrefix(f.session)
			require.NoError(t, receiver.UpdateSitePrefixesInDB(context.Background(), f.site.ID, report))
			got := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+result.ID+"?includeUsageStats=true", nil), http.StatusOK)
			require.Equal(t, cdbm.IPBlockStatusReady, got.Status)
			f.core.Metadata.Labels = []*corev1.Label{{Key: "existing", Value: cutil.GetPtr("preserved")}}
			updated := readTenantIPBlock(t, f.request(t, http.MethodPatch, "/"+result.ID, map[string]any{"name": "renamed"}), http.StatusOK)
			require.Equal(t, "renamed", updated.Name)
			require.Nil(t, updated.Description)
			require.Equal(t, f.deadlines[1], f.deadlines[2], "both Core calls must share one overall deadline")
			require.Equal(t, "preserved", f.core.Metadata.Labels[0].GetValue())
			// Inventory only changes lifecycle; it cannot undo an API metadata update.
			f.core.Metadata.Name = "old-name"
			require.NoError(t, receiver.UpdateSitePrefixesInDB(context.Background(), f.site.ID, report))
			updated = readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+result.ID, nil), http.StatusOK)
			require.Equal(t, "renamed", updated.Name)
			_, err = cdbm.NewSiteDAO(f.session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Config: &cdbm.SiteConfigUpdateInput{TenantSitePrefix: cutil.GetPtr(false)}})
			require.NoError(t, err)
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+result.ID, nil).Code)
			f.core.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY
			require.NoError(t, receiver.UpdateSitePrefixesInDB(context.Background(), f.site.ID, report))
			got = readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+result.ID, nil), http.StatusOK)
			require.Equal(t, cdbm.IPBlockStatusDeleting, got.Status)
			calls := len(f.calls)
			require.Equal(t, http.StatusConflict, f.request(t, http.MethodPatch, "/"+result.ID, map[string]any{"name": "too-late"}).Code)
			require.Len(t, f.calls, calls)

			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+result.ID, nil).Code)
		}},
		{http.MethodPost, "Core create timeout rolls back and inventory recovers the same ID", func(t *testing.T, f *tenantIPBlockFixture) {
			f.nextWait = true
			f.requestTimeout = time.Second
			require.Equal(t, http.StatusGatewayTimeout, f.request(t, http.MethodPost, "", f.body()).Code)
			f.requestTimeout = 0
			blocks, _, err := cdbm.NewIPBlockDAO(f.session).GetAll(context.Background(), nil, cdbm.IPBlockFilterInput{TenantIDs: []uuid.UUID{f.tenant.ID}}, cdbp.PageInput{}, nil)
			require.NoError(t, err)
			require.Empty(t, blocks)
			receiver := inventory.NewManageSitePrefix(f.session)
			require.NoError(t, receiver.UpdateSitePrefixesInDB(context.Background(), f.site.ID, &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, SitePrefixes: []*corev1.SitePrefix{f.core}}))
			result := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+f.core.Id.Value, nil), http.StatusOK)
			require.Equal(t, f.core.Id.Value, result.ID)
		}},
		{http.MethodPost, "definitive create rejection rolls back and releases admission", func(t *testing.T, f *tenantIPBlockFixture) {
			for _, failure := range []struct {
				name, kind, message string
				code                int
			}{
				{"provider deny prefix", swe.ErrTypeNICoInvalidArgument, "tenant SitePrefix 10.20.0.0/24 overlaps configured deny prefix 10.20.0.0/16", http.StatusBadRequest},
				{"conflict", swe.ErrTypeNICoAlreadyExists, "Core rejected create", http.StatusConflict},
				{"site isolation quota", swe.ErrTypeNICoResourceExhausted, "SitePrefix isolation rule limit reached: rules in use 231, after creation 232, limit 256", http.StatusTooManyRequests},
			} {
				t.Run(failure.name, func(t *testing.T) {
					f.nextError = tp.NewNonRetryableApplicationError(failure.message, failure.kind, nil)
					response := f.request(t, http.MethodPost, "", f.body())
					require.Equal(t, failure.code, response.Code, response.Body.String())
					require.NotContains(t, response.Body.String(), failure.message)
					_, err := cdbm.NewIPBlockDAO(f.session).GetByID(context.Background(), nil, f.lastCreatedID, nil)
					require.ErrorIs(t, err, cdb.ErrDoesNotExist)
					require.Nil(t, f.core, "definitive create rejection must not create a Core root")
				})
			}
			readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
		}},
		{http.MethodPost, "an admitted create remains removable after association removal", func(t *testing.T, f *tenantIPBlockFixture) {
			f.duringCore = func() {
				dao := cdbm.NewTenantSiteDAO(f.session)
				association, err := dao.GetByTenantIDAndSiteID(context.Background(), nil, f.tenant.ID, f.site.ID, nil)
				require.NoError(t, err)
				require.NoError(t, dao.Delete(context.Background(), nil, association.ID))
				f.duringCore = nil
			}
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			require.Equal(t, http.StatusBadRequest, f.request(t, http.MethodPatch, "/"+created.ID, map[string]any{"name": "after-removal"}).Code)
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
		}},
		{http.MethodPatch, "inconsistent Core ownership prevents metadata mutation", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			f.core.Config.TenantOrganizationId = cutil.GetPtr("other-tenant")
			calls := len(f.calls)
			response := f.request(t, http.MethodPatch, "/"+created.ID, map[string]any{"name": "must-not-change"})
			require.Equal(t, http.StatusBadGateway, response.Code, response.Body.String())
			var apiError map[string]any
			require.NoError(t, json.Unmarshal(response.Body.Bytes(), &apiError))
			require.Equal(t, "Core returned an inconsistent SitePrefix", apiError["message"])
			require.Nil(t, apiError["data"])
			require.Equal(t, []string{findTenantIPBlockMethod}, f.calls[calls:])
			require.Equal(t, created.Name, f.core.Metadata.Name)
			persisted := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+created.ID, nil), http.StatusOK)
			require.Equal(t, created.Name, persisted.Name)
			require.Equal(t, created.Status, persisted.Status)
		}},

		{http.MethodPatch, "concurrent Core update rejects the stale version and permits retry", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			f.beforeUpdate = func() { f.core.Version = "external-v2" }
			response := f.request(t, http.MethodPatch, "/"+created.ID, map[string]any{"name": "retried-name"})
			require.Equal(t, http.StatusPreconditionFailed, response.Code, response.Body.String())
			require.Equal(t, "external-v2", f.core.Version)
			require.Equal(t, created.Name, f.core.Metadata.Name)
			persisted := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+created.ID, nil), http.StatusOK)
			require.Equal(t, created.Name, persisted.Name)
			require.Equal(t, created.Status, persisted.Status)
			updated := readTenantIPBlock(t, f.request(t, http.MethodPatch, "/"+created.ID, map[string]any{"name": "retried-name"}), http.StatusOK)
			require.Equal(t, "retried-name", updated.Name)
			require.NotEqual(t, "external-v2", f.core.Version)
		}},
		{"", "mutation failures roll back REST changes", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			for _, mutation := range []struct {
				method, coreMethod string
				body               any
				errorType          string
				code               int
			}{
				{http.MethodPatch, updateTenantIPBlockMethod, map[string]any{"description": "committed-in-core"}, swe.ErrTypeNICoUnavailable, http.StatusServiceUnavailable},
				{http.MethodPatch, findTenantIPBlockMethod, map[string]any{"name": "lookup-failed"}, swe.ErrTypeNICoDenied, http.StatusForbidden},
				{http.MethodDelete, deleteTenantIPBlockMethod, nil, swe.ErrTypeNICoUnavailable, http.StatusServiceUnavailable},
			} {
				if mutation.method != method {
					continue
				}
				before := proto.Clone(f.core)
				f.commitOnError = mutation.errorType == swe.ErrTypeNICoUnavailable
				f.failMethod = mutation.coreMethod
				f.nextError = tp.NewNonRetryableApplicationError("private Core failure detail", mutation.errorType, nil)
				response := f.request(t, mutation.method, "/"+created.ID, mutation.body)
				require.Equal(t, mutation.code, response.Code, response.Body.String())
				require.NotContains(t, response.Body.String(), "private Core failure detail")
				if mutation.errorType != swe.ErrTypeNICoUnavailable {
					require.True(t, proto.Equal(before, f.core), "definitive rejection must not mutate Core")
				}
				persisted := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+created.ID, nil), http.StatusOK)
				require.Equal(t, created.Name, persisted.Name)
				require.Equal(t, created.Status, persisted.Status)
				if mutation.method == http.MethodPatch && mutation.errorType == swe.ErrTypeNICoUnavailable {
					require.NotEqual(t, before.(*corev1.SitePrefix).Version, f.core.Version, "uncertain committed update advances the version")
					f.failMethod = ""
					created = readTenantIPBlock(t, f.request(t, http.MethodPatch, "/"+created.ID, map[string]any{"name": "confirmed"}), http.StatusOK)
					require.Equal(t, "committed-in-core", *created.Description)
				}
			}
		}},
		{http.MethodPost, "privacy and allocation ownership", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			allocated, err := cdbm.NewIPBlockDAO(f.session).Create(context.Background(), nil, cdbm.IPBlockCreateInput{
				Name: "allocated", SiteID: f.site.ID, InfrastructureProviderID: f.site.InfrastructureProviderID, TenantID: &f.tenant.ID,
				Managed: cutil.GetPtr(true), Prefix: "10.22.0.0", PrefixLength: 24, ProtocolVersion: cdbm.IPBlockProtocolVersionV4,
				RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly, Status: cdbm.IPBlockStatusReady, CreatedBy: &f.user.ID})
			require.NoError(t, err)
			for _, id := range []string{allocated.ID.String(), uuid.NewString()} {
				for _, method := range []string{http.MethodPatch, http.MethodDelete} {
					require.Equal(t, http.StatusNotFound, f.request(t, method, "/"+id, map[string]any{"name": "forbidden"}).Code)
				}
			}
			listed := f.request(t, http.MethodGet, "?orderBy=NAME_ASC&pageSize=1", nil)
			require.Equal(t, http.StatusOK, listed.Code, listed.Body.String())
			var blocks []model.APIIPBlock
			require.NoError(t, json.Unmarshal(listed.Body.Bytes(), &blocks))
			require.Len(t, blocks, 1)
			require.True(t, blocks[0].Managed)
			require.Contains(t, listed.Header().Get("X-Pagination"), `"total":2`)
			listed = f.request(t, http.MethodGet, "?orderBy=NAME_ASC&pageSize=1&pageNumber=2", nil)
			require.Equal(t, http.StatusOK, listed.Code, listed.Body.String())
			require.NoError(t, json.Unmarshal(listed.Body.Bytes(), &blocks))
			require.Len(t, blocks, 1)
			require.Equal(t, created.ID, blocks[0].ID)
			require.False(t, blocks[0].Managed)
			calls := len(f.calls)
			f.org = "other-tenant"
			f.user = common.TestBuildUser(t, f.session, uuid.NewString(), f.org, []string{auth.TenantAdminRole})
			common.TestBuildTenant(t, f.session, "other-tenant", f.org, f.user)
			for _, method := range []string{http.MethodGet, http.MethodPatch, http.MethodDelete} {
				require.Equal(t, http.StatusNotFound, f.request(t, method, "/"+created.ID, map[string]any{"name": "forbidden"}).Code)
			}
			f.user = common.TestBuildUser(t, f.session, uuid.NewString(), f.org, []string{auth.ProviderAdminRole, auth.TenantAdminRole})
			common.TestBuildInfrastructureProvider(t, f.session, "other-provider", f.org, f.user)
			for _, method := range []string{http.MethodPatch, http.MethodDelete} {
				foreign := f.request(t, method, "/"+created.ID, map[string]any{"name": "forbidden"})
				unknown := f.request(t, method, "/"+uuid.NewString(), map[string]any{"name": "forbidden"})
				require.Equal(t, http.StatusNotFound, foreign.Code)
				require.Equal(t, unknown.Body.String(), foreign.Body.String())
			}
			f.org, f.user = "provider", f.providerUser
			require.Equal(t, http.StatusNotFound, f.request(t, http.MethodGet, "/"+created.ID, nil).Code)
			require.Len(t, f.calls, calls)
		}},
		{http.MethodPost, "capability association and strict request validation", func(t *testing.T, f *tenantIPBlockFixture) {
			_, err := cdbm.NewSiteDAO(f.session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Status: cutil.GetPtr(cdbm.SiteStatusError)})
			require.NoError(t, err)
			require.Equal(t, http.StatusBadRequest, f.request(t, http.MethodPost, "", f.body()).Code)
			require.Empty(t, f.calls)
			_, err = cdbm.NewSiteDAO(f.session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered)})
			require.NoError(t, err)
			_, err = f.session.DB.ExecContext(context.Background(), "UPDATE site SET config = NULL WHERE id = ?", f.site.ID)
			require.NoError(t, err)
			require.Equal(t, http.StatusPreconditionFailed, f.request(t, http.MethodPost, "", f.body()).Code)
			_, err = cdbm.NewSiteDAO(f.session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Config: &cdbm.SiteConfigUpdateInput{TenantSitePrefix: cutil.GetPtr(false)}})
			require.NoError(t, err)
			require.Equal(t, http.StatusPreconditionFailed, f.request(t, http.MethodPost, "", f.body()).Code)
			_, err = cdbm.NewSiteDAO(f.session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Config: &cdbm.SiteConfigUpdateInput{TenantSitePrefix: cutil.GetPtr(true)}})
			require.NoError(t, err)
			data, err := json.Marshal(f.body())
			require.NoError(t, err)
			var body map[string]any
			require.NoError(t, json.Unmarshal(data, &body))
			body["managed"] = true
			require.Equal(t, http.StatusBadRequest, f.request(t, http.MethodPost, "", body).Code)
			f.org = "unattached"
			f.user = common.TestBuildUser(t, f.session, uuid.NewString(), f.org, []string{auth.TenantAdminRole})
			common.TestBuildTenant(t, f.session, "unattached", f.org, f.user)
			existing := f.request(t, http.MethodPost, "", f.body())
			require.Equal(t, http.StatusBadRequest, existing.Code)
			unknownBody := f.body()
			unknownBody.SiteID = uuid.NewString()
			unknown := f.request(t, http.MethodPost, "", unknownBody)
			require.Equal(t, existing.Code, unknown.Code)
			require.Equal(t, existing.Body.String(), unknown.Body.String())
			require.Empty(t, f.calls)
		}},
		{http.MethodPost, "generated SDK tenant IP Block round trip", func(t *testing.T, f *tenantIPBlockFixture) {
			server := httptest.NewServer(f.router)
			defer server.Close()
			cfg := sdk.NewConfiguration()
			cfg.Servers = sdk.ServerConfigurations{{URL: server.URL}}
			client := sdk.NewAPIClient(cfg)
			capabilities := sdk.NewSiteCapabilitiesUpdateRequest()
			capabilities.SetTenantSitePrefix(false)
			siteUpdate := sdk.NewSiteUpdateRequest()
			siteUpdate.SetCapabilities(*capabilities)
			_, response, err := client.SiteAPI.UpdateSite(context.Background(), f.org, f.site.ID.String()).SiteUpdateRequest(*siteUpdate).Execute()
			require.Error(t, err)
			require.Equal(t, http.StatusBadRequest, response.StatusCode)
			tenantUser := f.user
			f.user = f.providerUser
			for _, enabled := range []bool{false, true} {
				capabilities.SetTenantSitePrefix(enabled)
				siteUpdate.SetCapabilities(*capabilities)
				site, _, err := client.SiteAPI.UpdateSite(context.Background(), "provider", f.site.ID.String()).SiteUpdateRequest(*siteUpdate).Execute()
				require.NoError(t, err)
				storedCapabilities := site.GetCapabilities()
				require.Equal(t, enabled, storedCapabilities.GetTenantSitePrefix())
				persisted, err := cdbm.NewSiteDAO(f.session).GetByID(context.Background(), nil, f.site.ID, nil, false, false)
				require.NoError(t, err)
				require.Equal(t, enabled, persisted.Config.TenantSitePrefix)
			}
			f.user = tenantUser
			request := sdk.NewIpBlockCreateRequest("sdk-private", f.site.ID.String(), cdbm.IPBlockRoutingTypeDatacenterOnly, "10.21.0.0", 24, cdbm.IPBlockProtocolVersionV4)
			created, response, err := client.IPBlockAPI.CreateIpblock(context.Background(), f.org).IpBlockCreateRequest(*request).Execute()
			require.NoError(t, err)
			require.Equal(t, http.StatusCreated, response.StatusCode)
			require.False(t, created.GetManaged())
			require.Equal(t, "/v2/org/tenant/nico/ipblock/"+created.GetId(), response.Header.Get("Location"))
			got, _, err := client.IPBlockAPI.GetIpblock(context.Background(), f.org, created.GetId()).Execute()
			require.NoError(t, err)
			require.Equal(t, created.GetId(), got.GetId())
			update := sdk.NewIpBlockUpdateRequest()
			update.SetName("sdk-renamed")
			got, _, err = client.IPBlockAPI.UpdateIpblock(context.Background(), f.org, created.GetId()).IpBlockUpdateRequest(*update).Execute()
			require.NoError(t, err)
			require.Equal(t, "sdk-renamed", got.GetName())
			_, response, err = client.IPBlockAPI.DeleteIpblock(context.Background(), f.org, created.GetId()).Execute()
			require.NoError(t, err)
			require.Equal(t, http.StatusAccepted, response.StatusCode)
		}},
		{http.MethodDelete, "retirement survives removal of the last Allocation", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			allocation := common.TestBuildAllocation(t, f.session, f.site, f.tenant, "last-allocation", f.providerUser)
			tenantUser := f.user
			f.user = f.providerUser
			req := httptest.NewRequest(http.MethodDelete, "/v2/org/provider/nico/allocation/"+allocation.ID.String(), nil)
			rec := httptest.NewRecorder()
			f.router.ServeHTTP(rec, req)
			require.Equal(t, http.StatusAccepted, rec.Code, rec.Body.String())
			_, err := cdbm.NewTenantSiteDAO(f.session).GetByTenantIDAndSiteID(context.Background(), nil, f.tenant.ID, f.site.ID, nil)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			f.user = tenantUser
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
			require.Equal(t, corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING, f.core.Status.LifecycleState)
		}},
		{http.MethodPost, "validation includes actionable field errors", func(t *testing.T, f *tenantIPBlockFixture) {
			body := f.body()
			body.Prefix = "8.8.8.0"
			response := f.request(t, http.MethodPost, "", body)
			require.Equal(t, http.StatusBadRequest, response.Code)
			require.Contains(t, response.Body.String(), "RFC1918")
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			response = f.request(t, http.MethodPatch, "/"+created.ID, map[string]any{"description": strings.Repeat("x", 1025)})
			require.Equal(t, http.StatusBadRequest, response.Code)
			require.Contains(t, response.Body.String(), "description")
		}},
		{http.MethodPost, "Location follows the mounted route", func(t *testing.T, f *tenantIPBlockFixture) {
			data, err := json.Marshal(f.body())
			require.NoError(t, err)
			req := httptest.NewRequest(http.MethodPost, "/custom/org/tenant/carbide/ipblock", strings.NewReader(string(data)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			f.router.ServeHTTP(rec, req)
			created := readTenantIPBlock(t, rec, http.StatusCreated)
			require.Equal(t, req.URL.Path+"/"+created.ID, rec.Header().Get("Location"))
		}},
		{http.MethodDelete, "lost first retirement response remains retryable after Core removal", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			f.nextWait = true
			f.requestTimeout = time.Second
			require.Equal(t, http.StatusGatewayTimeout, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
			f.requestTimeout = 0
			got := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+created.ID, nil), http.StatusOK)
			require.Equal(t, created.Status, got.Status)
			f.nextError = tp.NewNonRetryableApplicationError("already absent", swe.ErrTypeNICoObjectNotFound, nil)
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
			got = readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+created.ID, nil), http.StatusOK)
			require.Equal(t, cdbm.IPBlockStatusDeleting, got.Status)
			require.NotEmpty(t, got.StatusHistory)
			require.Equal(t, cutil.GetPtr("Core reported SitePrefix absent; waiting for inventory"), got.StatusHistory[0].Message)
		}},
		{http.MethodDelete, "restored deletion remains removable through the public API", func(t *testing.T, f *tenantIPBlockFixture) {
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
			id := uuid.MustParse(created.ID)
			require.NoError(t, cdbm.NewIPBlockDAO(f.session).Delete(context.Background(), nil, id))
			_, err := f.session.DB.ExecContext(context.Background(), "UPDATE ip_block SET deleted = ? WHERE id = ?", time.Now().Add(-time.Hour), id)
			require.NoError(t, err)
			f.core.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY
			receiver := inventory.NewManageSitePrefix(f.session)
			require.NoError(t, receiver.UpdateSitePrefixesInDB(context.Background(), f.site.ID, &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, SitePrefixes: []*corev1.SitePrefix{f.core}}))
			restored := readTenantIPBlock(t, f.request(t, http.MethodGet, "/"+created.ID, nil), http.StatusOK)
			require.Equal(t, cdbm.IPBlockStatusDeleting, restored.Status)
			calls := len(f.calls)
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
			require.Len(t, f.calls, calls+1)
			require.Equal(t, corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING, f.core.Status.LifecycleState)
			f.nextError = tp.NewNonRetryableApplicationError("already absent", swe.ErrTypeNICoObjectNotFound, nil)
			require.Equal(t, http.StatusAccepted, f.request(t, http.MethodDelete, "/"+created.ID, nil).Code)
		}},
	} {
		if row.method != method && (row.method != "" || method == http.MethodPost) {
			continue
		}
		t.Run(row.name, func(t *testing.T) { row.check(t, newTenantIPBlockFixture(t)) })
	}
}

// Exercise the public Site handler and generated SDK error contract while a
// tenant transaction owns the lifecycle fence.
func checkTenantIPBlockSiteConflict(t *testing.T, method string) {
	t.Helper()
	f := newTenantIPBlockFixture(t)
	tx, err := cdb.BeginTx(context.Background(), f.session, nil)
	require.NoError(t, err)
	defer func() { _ = tx.Rollback() }()
	_, err = cdbm.NewSiteDAO(f.session).GetByIDForTenantIPBlockMutation(context.Background(), tx, f.site.ID)
	require.NoError(t, err)
	f.user = f.providerUser
	var blockerPID int
	require.NoError(t, tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(context.Background(), &blockerPID))
	requestCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	f.siteRequestContext = requestCtx
	server := httptest.NewServer(f.router)
	defer func() { cancel(); server.Close() }()
	cfg := sdk.NewConfiguration()
	cfg.Servers = sdk.ServerConfigurations{{URL: server.URL}}
	client := sdk.NewAPIClient(cfg)
	type result struct {
		response *http.Response
		err      error
	}
	finished := make(chan result, 1)
	go func() {
		var response *http.Response
		var err error
		if method == http.MethodPatch {
			capabilities := sdk.NewSiteCapabilitiesUpdateRequest()
			capabilities.SetTenantSitePrefix(false)
			request := sdk.NewSiteUpdateRequest()
			request.SetCapabilities(*capabilities)
			_, response, err = client.SiteAPI.UpdateSite(context.Background(), "provider", f.site.ID.String()).SiteUpdateRequest(*request).Execute()
		} else {
			_, response, err = client.SiteAPI.DeleteSite(context.Background(), "provider", f.site.ID.String()).Execute()
		}
		finished <- result{response, err}
	}()
	// Cancel only after the provider request reaches the lifecycle lock; a short
	// request timeout can expire in unrelated setup on a loaded test machine.
	require.Eventually(t, func() bool {
		var waiting bool
		err := f.session.DB.NewSelect().ColumnExpr("EXISTS (SELECT 1 FROM pg_stat_activity WHERE ? = ANY(pg_blocking_pids(pid)))", blockerPID).Scan(context.Background(), &waiting)
		return err == nil && waiting
	}, 5*time.Second, 10*time.Millisecond)
	cancel()
	got := <-finished
	response, err := got.response, got.err

	require.Error(t, err)
	require.Equal(t, http.StatusConflict, response.StatusCode)
	var apiErr *sdk.GenericOpenAPIError
	require.ErrorAs(t, err, &apiErr)
	require.IsType(t, sdk.NICoAPIError{}, apiErr.Model())
}

func checkTenantIPBlockDispatch(t *testing.T, method string) {
	t.Helper()
	for _, row := range []struct {
		name  string
		roles []string
	}{
		{"tenant admin", []string{auth.TenantAdminRole}},
		{"provider and tenant admin", []string{auth.ProviderAdminRole, auth.TenantAdminRole}},
	} {
		t.Run(row.name, func(t *testing.T) {
			f := newTenantIPBlockFixture(t)
			created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
			f.user = common.TestBuildUser(t, f.session, uuid.NewString(), f.org, row.roles)
			switch method {
			case http.MethodPost:
				response := f.request(t, method, "", f.body())
				if len(row.roles) == 2 {
					require.Equal(t, http.StatusBadRequest, response.Code)
					require.Contains(t, response.Body.String(), "Failed to retrieve infrastructure provider")
					require.Len(t, f.calls, 1)
				} else {
					readTenantIPBlock(t, response, http.StatusCreated)
					require.Len(t, f.calls, 2)
				}
			case http.MethodPatch:
				updated := readTenantIPBlock(t, f.request(t, method, "/"+created.ID, map[string]any{"name": "dispatched"}), http.StatusOK)
				require.Equal(t, "dispatched", updated.Name)
				require.Equal(t, updateTenantIPBlockMethod, f.calls[len(f.calls)-1])
			case http.MethodDelete:
				require.Equal(t, http.StatusAccepted, f.request(t, method, "/"+created.ID, nil).Code)
				require.Equal(t, deleteTenantIPBlockMethod, f.calls[len(f.calls)-1])
			}
		})
	}
}

func checkTenantIPBlockLogs(t *testing.T, method, operation string) {
	t.Helper()
	f := newTenantIPBlockFixture(t)
	path := ""
	var body any = f.body()
	want := http.StatusCreated
	if method != http.MethodPost {
		created := readTenantIPBlock(t, f.request(t, http.MethodPost, "", f.body()), http.StatusCreated)
		path = "/" + created.ID
		body, want = map[string]any{"name": "renamed"}, http.StatusOK
		if method == http.MethodDelete {
			body, want = nil, http.StatusAccepted
		}
	}
	var logs bytes.Buffer
	previous := log.Logger
	log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
	t.Cleanup(func() { log.Logger = previous })
	response := f.request(t, method, path, body)
	require.Equal(t, want, response.Code, response.Body.String())
	starts, finishes := 0, 0
	decoder := json.NewDecoder(&logs)
	for decoder.More() {
		var event map[string]any
		require.NoError(t, decoder.Decode(&event))
		if event["Model"] != "IPBlock" {
			continue
		}
		if event["message"] == "started API handler" {
			starts++
			require.Equal(t, operation, event["Handler"])
		}
		if event["message"] == "finishing API handler" {
			finishes++
			require.Equal(t, operation, event["Handler"])
		}
	}
	require.Equal(t, 1, starts)
	require.Equal(t, 1, finishes)
}
