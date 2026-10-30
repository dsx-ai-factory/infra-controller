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

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	temporalEnums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestGetVPCRoutingProfileHandler_Handle(t *testing.T) {
	cases := []struct {
		name          string
		profile       *string
		retained      *corev1.VpcRetainedVniAllocation
		activeVni     uint32
		expectedState string
	}{
		{
			name:          "authoritative state uses REST identity and normalized profile",
			profile:       cutil.GetPtr("EXTERNAL"),
			retained:      &corev1.VpcRetainedVniAllocation{PoolName: "vpc-vni", Vni: 10100},
			activeVni:     51000,
			expectedState: `{"vpcId":%q,"version":"V7-T1761856992374052","routingProfile":"external","activeVni":51000,"retainedAllocation":{"poolName":"vpc-vni","vni":10100}}`,
		},
		{
			name:          "inspection preserves absent profile and allocation",
			expectedState: `{"vpcId":%q,"version":"V7-T1761856992374052","routingProfile":null,"activeVni":0,"retainedAllocation":null}`,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			authorizationDeadline := f.captureAuthorizationDeadline(t)
			state := f.routingState()
			state.RoutingProfileType = tc.profile
			state.RetainedAllocation = tc.retained
			state.ActiveVni = tc.activeVni
			f.expectProxy(t, corev1.Forge_GetVpcRoutingState_FullMethodName, state, nil, nil)

			rec := f.request(t, context.Background(), http.MethodGet, "")
			assert.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
			assert.JSONEq(t, fmt.Sprintf(tc.expectedState, f.vpc.ID.String()), rec.Body.String())
			require.Len(t, f.proxiedReqs, 1)
			var req corev1.VpcRoutingStateRequest
			require.NoError(t, protojson.Unmarshal(f.proxiedReqs[0].RequestJSON, &req))
			assert.Equal(t, f.controllerID.String(), req.GetId().GetValue())
			require.Len(t, f.deadlines, 1)
			assert.Equal(t, *authorizationDeadline, f.deadlines[0], "the proxy must preserve the authorization deadline")
		})
	}
	t.Run("authorization", func(t *testing.T) {
		assertVPCRoutingProfileProviderOnly(t, http.MethodGet, "")
	})
	accessCases := []struct {
		name   string
		setup  func(*testing.T, *vpcRoutingProfileHandlerFixture)
		status int
	}{
		{
			name: "missing user",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				f.user = nil
			},
			status: http.StatusInternalServerError,
		},
		{
			name: "dual-role user in tenant-only org has no Provider",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				f.org = "tenant-only-org-" + uuid.NewString()
				f.user = common.TestBuildUser(t, f.dbSession, uuid.NewString(), f.org, []string{authz.ProviderAdminRole, authz.TenantAdminRole})
				tenant := common.TestBuildTenant(t, f.dbSession, "Tenant Only", f.org, f.user)
				require.NotNil(t, tenant)
			},
			status: http.StatusBadRequest,
		},
		{
			name: "malformed VPC identity",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				f.vpcID = "not-a-uuid"
			},
			status: http.StatusBadRequest,
		},
		{
			name: "missing VPC",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				f.vpcID = uuid.NewString()
			},
			status: http.StatusNotFound,
		},
		{
			name: "VPC belongs to another provider",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				provider := common.TestBuildInfrastructureProvider(t, f.dbSession, "Other Provider", uuid.NewString(), f.user)
				_, err := f.dbSession.DB.NewUpdate().Model(f.vpc).Set("infrastructure_provider_id = ?", provider.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name: "VPC Site belongs to another provider",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				provider := common.TestBuildInfrastructureProvider(t, f.dbSession, "Other Provider", uuid.NewString(), f.user)
				_, err := f.dbSession.DB.NewUpdate().Model(f.site).Set("infrastructure_provider_id = ?", provider.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
			status: http.StatusForbidden,
		},
		{
			name: "unregistered Site",
			setup: func(t *testing.T, f *vpcRoutingProfileHandlerFixture) {
				_, err := cdbm.NewSiteDAO(f.dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Status: cutil.GetPtr(cdbm.SiteStatusPending)})
				require.NoError(t, err)
			},
			status: http.StatusBadRequest,
		},
	}
	for _, tc := range accessCases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			tc.setup(t, f)
			rec := f.request(t, context.Background(), http.MethodGet, "")
			assert.Equal(t, tc.status, rec.Code, rec.Body.String())
			assert.Empty(t, f.proxiedReqs)
		})
	}
	readCases := []struct {
		name   string
		change func(*corev1.VpcRoutingState)
		getErr error
		status int
	}{
		{name: "missing authoritative identity", change: func(s *corev1.VpcRoutingState) { s.Id = nil }, status: http.StatusInternalServerError},
		{name: "old Core reports unsupported method", getErr: tp.NewNonRetryableApplicationError("unsupported method", swe.ErrTypeNICoUnimplemented, nil), status: http.StatusNotImplemented},
	}
	for _, tc := range readCases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			state := f.routingState()
			if tc.change != nil {
				tc.change(state)
			}
			f.expectProxy(t, corev1.Forge_GetVpcRoutingState_FullMethodName, state, tc.getErr, nil)
			rec := f.request(t, context.Background(), http.MethodGet, "")
			assert.Equal(t, tc.status, rec.Code, rec.Body.String())
			assert.NotContains(t, rec.Body.String(), "may have committed")
			require.Len(t, f.proxiedReqs, 1)
		})
	}
}

func TestUpdateVPCRoutingProfileHandler_Handle(t *testing.T) {
	cases := []struct {
		name        string
		body        string
		coreProfile string
		apiProfile  string
		exactVni    *uint32
	}{
		{
			name:        "normalizes alias and forwards exact VNI",
			body:        `{"routingProfile":"external","vni":51000,"ifVersionMatch":"V1-T1"}`,
			coreProfile: "EXTERNAL",
			apiProfile:  "external",
			exactVni:    cutil.GetPtr(uint32(51000)),
		},
		{
			name:        "preserves configured profile and delegates VNI selection",
			body:        `{"routingProfile":"tenant-edge"}`,
			coreProfile: "tenant-edge",
			apiProfile:  "tenant-edge",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			observed := f.routingState()
			changed := f.routingState()
			changed.Version = "V8-T1761856992374053"
			changed.RoutingProfileType = cutil.GetPtr(tc.coreProfile)
			changed.ActiveVni = 51000
			changed.RetainedAllocation = &corev1.VpcRetainedVniAllocation{PoolName: "vpc-vni", Vni: observed.ActiveVni}
			f.expectProxy(t, corev1.Forge_GetVpcRoutingState_FullMethodName, observed, nil, nil)
			f.expectProxy(t, corev1.Forge_ChangeVpcRoutingProfile_FullMethodName, changed, nil, nil)

			rec := f.request(t, context.Background(), http.MethodPatch, tc.body)
			assert.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
			assert.JSONEq(t, fmt.Sprintf(`{"vpcId":%q,"version":"V8-T1761856992374053","routingProfile":%q,"activeVni":51000,"retainedAllocation":{"poolName":"vpc-vni","vni":10100}}`, f.vpc.ID.String(), tc.apiProfile), rec.Body.String())
			require.Len(t, f.proxiedReqs, 2)
			assert.Equal(t, corev1.Forge_GetVpcRoutingState_FullMethodName, f.proxiedReqs[0].FullMethod)
			assert.Equal(t, corev1.Forge_ChangeVpcRoutingProfile_FullMethodName, f.proxiedReqs[1].FullMethod)
			var read corev1.VpcRoutingStateRequest
			require.NoError(t, protojson.Unmarshal(f.proxiedReqs[0].RequestJSON, &read))
			assert.Equal(t, f.controllerID.String(), read.GetId().GetValue())
			var change corev1.VpcChangeRoutingProfileRequest
			require.NoError(t, protojson.Unmarshal(f.proxiedReqs[1].RequestJSON, &change))
			assert.Equal(t, f.controllerID.String(), change.GetId().GetValue())
			require.NotNil(t, change.IfVersionMatch)
			assert.Equal(t, observed.Version, *change.IfVersionMatch)
			assert.Equal(t, tc.coreProfile, change.RoutingProfileType)
			assert.Equal(t, tc.exactVni, change.Vni)
			require.Len(t, f.deadlines, 2)
			assert.Equal(t, f.deadlines[0], f.deadlines[1], "the mutation must not restart the caller timeout")
			assert.Positive(t, time.Until(f.deadlines[0]))
			assert.LessOrEqual(t, time.Until(f.deadlines[0]), cutil.WorkflowContextTimeout)
		})
	}
	t.Run("authorization", func(t *testing.T) {
		assertVPCRoutingProfileProviderOnly(t, http.MethodPatch, `{"routingProfile":"external"}`)
	})
	failureCases := []struct {
		name             string
		body             string
		readErr          error
		invalidRead      bool
		cancelAfterRead  bool
		mutationErr      error
		changeAck        func(*corev1.VpcRoutingState)
		status           int
		calls            int
		mayHaveCommitted bool
	}{
		{name: "invalid request cannot read or mutate", body: `{"routingProfile":"external","vni":0}`, status: http.StatusBadRequest},
		{name: "failed read cannot mutate", readErr: tp.NewNonRetryableApplicationError("Core unavailable", swe.ErrTypeNICoUnavailable, nil), status: http.StatusServiceUnavailable, calls: 1},
		{name: "invalid read cannot mutate", invalidRead: true, status: http.StatusInternalServerError, calls: 1},
		{name: "cancellation after read cannot mutate", cancelAfterRead: true, status: http.StatusGatewayTimeout, calls: 1},
		{name: "stale version is returned without reread or retry", mutationErr: tp.NewNonRetryableApplicationError("stale VPC version", swe.ErrTypeNICoFailedPrecondition, nil), status: http.StatusPreconditionFailed, calls: 2},
		{name: "unsupported mutation remains 501", mutationErr: tp.NewNonRetryableApplicationError("unsupported method", swe.ErrTypeNICoUnimplemented, nil), status: http.StatusNotImplemented, calls: 2},
		{name: "ambiguous timeout is returned without retry", mutationErr: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), status: http.StatusGatewayTimeout, calls: 2, mayHaveCommitted: true},
		{name: "wrong acknowledged exact VNI", changeAck: func(s *corev1.VpcRoutingState) { s.ActiveVni = 51001 }, status: http.StatusInternalServerError, calls: 2, mayHaveCommitted: true},
	}
	for _, tc := range failureCases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if tc.calls > 0 {
				observed := f.routingState()
				if tc.invalidRead {
					observed.Version = ""
				}
				var afterGet func()
				if tc.cancelAfterRead {
					// Return a successful read, then cancel before the handler can submit the change.
					afterGet = cancel
				}
				f.expectProxy(t, corev1.Forge_GetVpcRoutingState_FullMethodName, observed, tc.readErr, afterGet)
			}
			if tc.calls == 2 {
				changed := f.routingState()
				changed.Version = "V8-T1761856992374053"
				changed.RoutingProfileType = cutil.GetPtr("EXTERNAL")
				changed.ActiveVni = 51000
				changed.RetainedAllocation = &corev1.VpcRetainedVniAllocation{PoolName: "vpc-vni", Vni: 10100}
				if tc.changeAck != nil {
					tc.changeAck(changed)
				}
				f.expectProxy(t, corev1.Forge_ChangeVpcRoutingProfile_FullMethodName, changed, tc.mutationErr, nil)
			}
			body := tc.body
			if body == "" {
				body = `{"routingProfile":"external","vni":51000}`
			}
			rec := f.request(t, ctx, http.MethodPatch, body)
			assert.Equal(t, tc.status, rec.Code, rec.Body.String())
			require.Len(t, f.proxiedReqs, tc.calls)
			if tc.mayHaveCommitted {
				assert.Contains(t, rec.Body.String(), "may have committed")
				assert.Contains(t, rec.Body.String(), "inspect VPC routing state before submitting another request")
			} else {
				assert.NotContains(t, rec.Body.String(), "may have committed")
			}
			if tc.calls == 2 {
				var change corev1.VpcChangeRoutingProfileRequest
				require.NoError(t, protojson.Unmarshal(f.proxiedReqs[1].RequestJSON, &change))
				assert.Equal(t, "V7-T1761856992374052", change.GetIfVersionMatch())
			}
		})
	}
}

func TestReleaseVPCInactiveVniHandler_Handle(t *testing.T) {
	t.Run("forwards observed version and exact allocation without reading again", func(t *testing.T) {
		f := newVPCRoutingProfileHandlerFixture(t)
		authorizationDeadline := f.captureAuthorizationDeadline(t)
		f.expectProxy(t, corev1.Forge_ReleaseVpcInactiveVni_FullMethodName, f.releaseResult(), nil, nil)

		rec := f.request(t, context.Background(), http.MethodPost, `{"ifVersionMatch":"V8-T1761856992374053","expectedInactiveVni":10100}`)
		assert.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
		assert.JSONEq(t, fmt.Sprintf(`{"vpcId":%q,"version":"V9-T1761856992374054","routingProfile":"external","activeVni":51000,"releasedInactiveVni":10100}`, f.vpc.ID.String()), rec.Body.String())
		require.Len(t, f.proxiedReqs, 1)
		assert.Equal(t, corev1.Forge_ReleaseVpcInactiveVni_FullMethodName, f.proxiedReqs[0].FullMethod)
		var release corev1.VpcReleaseInactiveVniRequest
		require.NoError(t, protojson.Unmarshal(f.proxiedReqs[0].RequestJSON, &release))
		assert.Equal(t, f.controllerID.String(), release.GetId().GetValue())
		require.NotNil(t, release.IfVersionMatch)
		assert.Equal(t, "V8-T1761856992374053", *release.IfVersionMatch)
		require.NotNil(t, release.ExpectedInactiveVni)
		assert.Equal(t, uint32(10100), *release.ExpectedInactiveVni)
		require.Len(t, f.deadlines, 1)
		assert.Equal(t, *authorizationDeadline, f.deadlines[0], "the proxy must preserve the authorization deadline")
	})
	t.Run("authorization", func(t *testing.T) {
		assertVPCRoutingProfileProviderOnly(t, http.MethodPost, `{"ifVersionMatch":"V8-T1761856992374053","expectedInactiveVni":10100}`)
	})
	cases := []struct {
		name             string
		body             string
		getErr           error
		changeAck        func(*corev1.VpcReleaseInactiveVniResult)
		cancelAfterBind  bool
		status           int
		calls            int
		mayHaveCommitted bool
	}{
		{name: "missing observed version cannot release", body: `{"expectedInactiveVni":10100}`, status: http.StatusBadRequest},
		{name: "cancellation after binding cannot release", cancelAfterBind: true, status: http.StatusGatewayTimeout},
		{name: "stale version is returned without refresh or retry", getErr: tp.NewNonRetryableApplicationError("stale VPC version", swe.ErrTypeNICoFailedPrecondition, nil), status: http.StatusPreconditionFailed, calls: 1},
		{name: "ambiguous timeout is returned without retry", getErr: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), status: http.StatusGatewayTimeout, calls: 1, mayHaveCommitted: true},
		{name: "wrong released VNI is not acknowledged", changeAck: func(r *corev1.VpcReleaseInactiveVniResult) { r.ReleasedInactiveVni = 10101 }, status: http.StatusInternalServerError, calls: 1, mayHaveCommitted: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if tc.cancelAfterBind {
				f.binder = vpcRoutingProfileBinder(func(req interface{}, c echo.Context) error {
					err := (&echo.DefaultBinder{}).Bind(req, c)
					require.NoError(t, err)
					cancel()
					return err
				})
			}
			if tc.calls != 0 {
				result := f.releaseResult()
				if tc.changeAck != nil {
					tc.changeAck(result)
				}
				f.expectProxy(t, corev1.Forge_ReleaseVpcInactiveVni_FullMethodName, result, tc.getErr, nil)
			}
			body := tc.body
			if body == "" {
				body = `{"ifVersionMatch":"V8-T1761856992374053","expectedInactiveVni":10100}`
			}
			rec := f.request(t, ctx, http.MethodPost, body)
			assert.Equal(t, tc.status, rec.Code, rec.Body.String())
			require.Len(t, f.proxiedReqs, tc.calls)
			if tc.mayHaveCommitted {
				assert.Contains(t, rec.Body.String(), "inspect VPC routing state before submitting another request")
			} else {
				assert.NotContains(t, rec.Body.String(), "may have committed")
			}
			if tc.calls == 1 {
				var release corev1.VpcReleaseInactiveVniRequest
				require.NoError(t, protojson.Unmarshal(f.proxiedReqs[0].RequestJSON, &release))
				assert.Equal(t, "V8-T1761856992374053", release.GetIfVersionMatch())
				assert.Equal(t, uint32(10100), release.GetExpectedInactiveVni())
			}
		})
	}
}

type vpcRoutingProfileHandlerFixture struct {
	dbSession    *cdb.Session
	org          string
	user         *cdbm.User
	vpc          *cdbm.Vpc
	site         *cdbm.Site
	vpcID        string
	controllerID uuid.UUID
	scp          *sc.ClientPool
	tsc          *tmocks.Client
	proxiedReqs  []grpcproxy.Request
	deadlines    []time.Time
	binder       echo.Binder
}

// vpcRoutingProfileQueryHook inspects the context used for authorization queries.
type vpcRoutingProfileQueryHook func(context.Context)

// BeforeQuery observes the query context without changing it.
func (h vpcRoutingProfileQueryHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	h(ctx)
	return ctx
}

// AfterQuery leaves the query result unchanged.
func (vpcRoutingProfileQueryHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// captureAuthorizationDeadline records the deadline shared by authorization and the proxy.
func (f *vpcRoutingProfileHandlerFixture) captureAuthorizationDeadline(t *testing.T) *time.Time {
	t.Helper()
	var deadline time.Time
	f.dbSession.DB.AddQueryHook(vpcRoutingProfileQueryHook(func(ctx context.Context) {
		queryDeadline, ok := ctx.Deadline()
		require.True(t, ok, "authorization queries must have a deadline")
		assert.Positive(t, time.Until(queryDeadline))
		assert.LessOrEqual(t, time.Until(queryDeadline), cutil.WorkflowContextTimeout)
		if deadline.IsZero() {
			deadline = queryDeadline
		}
		assert.Equal(t, deadline, queryDeadline)
	}))
	return &deadline
}

// vpcRoutingProfileBinder lets a test cancel the request after normal binding.
type vpcRoutingProfileBinder func(interface{}, echo.Context) error

// Bind invokes the test's binding callback.
func (b vpcRoutingProfileBinder) Bind(req interface{}, c echo.Context) error {
	return b(req, c)
}

func newVPCRoutingProfileHandlerFixture(t *testing.T) *vpcRoutingProfileHandlerFixture {
	t.Helper()
	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)
	org := "routing-profile-org-" + uuid.NewString()
	user := common.TestBuildUser(t, dbSession, uuid.NewString(), org, []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "Routing Profile Provider", org, user)
	tenant := common.TestBuildTenant(t, dbSession, "Routing Profile Tenant", org, user)
	site := common.TestBuildSite(t, dbSession, provider, "Routing Profile Site", user)
	_, err := cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	controllerID := uuid.New()
	vpc := common.TestBuildVPC(t, dbSession, "Routing Profile VPC", provider, tenant, site, &controllerID, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, user)
	require.NotEqual(t, vpc.ID, controllerID)
	tsc := &tmocks.Client{}
	tsc.Test(t)
	t.Cleanup(func() { tsc.AssertExpectations(t) })
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc
	return &vpcRoutingProfileHandlerFixture{
		dbSession:    dbSession,
		org:          org,
		user:         user,
		vpc:          vpc,
		site:         site,
		vpcID:        vpc.ID.String(),
		controllerID: controllerID,
		scp:          scp,
		tsc:          tsc,
	}
}

func (f *vpcRoutingProfileHandlerFixture) routingState() *corev1.VpcRoutingState {
	return &corev1.VpcRoutingState{
		Id:                 &corev1.VpcId{Value: f.controllerID.String()},
		Version:            "V7-T1761856992374052",
		RoutingProfileType: cutil.GetPtr("INTERNAL"),
		ActiveVni:          10100,
	}
}

func (f *vpcRoutingProfileHandlerFixture) releaseResult() *corev1.VpcReleaseInactiveVniResult {
	return &corev1.VpcReleaseInactiveVniResult{
		Vpc: &corev1.Vpc{
			Id:      &corev1.VpcId{Value: f.controllerID.String()},
			Version: "V9-T1761856992374054",
			Config:  &corev1.VpcConfig{RoutingProfileType: cutil.GetPtr("EXTERNAL")},
			Status:  &corev1.VpcStatus{Vni: cutil.GetPtr(uint32(51000))},
		},
		ReleasedInactiveVni: 10100,
	}
}

func (f *vpcRoutingProfileHandlerFixture) expectProxy(t *testing.T, method string, response proto.Message, getErr error, afterGet func()) {
	t.Helper()
	wrun := &tmocks.WorkflowRun{}
	wrun.Test(t)
	t.Cleanup(func() { wrun.AssertExpectations(t) })
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response != nil {
			responseJSON, err := protojson.Marshal(response)
			require.NoError(t, err)
			args.Get(1).(*grpcproxy.Response).ResponseJSON = responseJSON
		}
		if afterGet != nil {
			afterGet()
		}
	}).Return(getErr).Once()
	f.tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool { return req.FullMethod == method }),
	).Run(func(args mock.Arguments) {
		req := args.Get(3).(grpcproxy.Request)
		assert.Empty(t, req.EncryptedSecrets)
		f.proxiedReqs = append(f.proxiedReqs, req)
		deadline, ok := args.Get(0).(context.Context).Deadline()
		require.True(t, ok)
		f.deadlines = append(f.deadlines, deadline)
	}).Return(wrun, nil).Once()
}

func (f *vpcRoutingProfileHandlerFixture) request(t *testing.T, ctx context.Context, method, body string) *httptest.ResponseRecorder {
	t.Helper()
	path := "/vpc/:id/routing-profile"
	if method == http.MethodPost {
		path += "/release-inactive-vni"
	}
	req := httptest.NewRequest(method, "/", strings.NewReader(body)).WithContext(ctx)
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	e := echo.New()
	if f.binder != nil {
		e.Binder = f.binder
	}
	c := e.NewContext(req, rec)
	c.SetPath(path)
	c.SetParamNames("orgName", "id")
	c.SetParamValues(f.org, f.vpcID)
	if f.user != nil {
		c.Set("user", f.user)
	}
	var err error
	switch method {
	case http.MethodGet:
		err = NewGetVPCRoutingProfileHandler(f.dbSession, f.scp).Handle(c)
	case http.MethodPatch:
		err = NewUpdateVPCRoutingProfileHandler(f.dbSession, f.scp).Handle(c)
	case http.MethodPost:
		err = NewReleaseVPCInactiveVniHandler(f.dbSession, f.scp).Handle(c)
	default:
		t.Fatalf("unsupported routing profile test method %q", method)
	}
	require.NoError(t, err)
	return rec
}

func assertVPCRoutingProfileProviderOnly(t *testing.T, method, body string) {
	t.Helper()
	for _, role := range []string{authz.ProviderViewerRole, authz.TenantAdminRole} {
		t.Run(role, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
			org := f.user.OrgData[f.org]
			org.Roles = []string{role}
			f.user.OrgData[f.org] = org
			rec := f.request(t, context.Background(), method, body)
			assert.Equal(t, http.StatusForbidden, rec.Code, rec.Body.String())
			assert.Empty(t, f.proxiedReqs)
		})
	}
}

func TestGetVPCRoutingProfileDefinitionHandler_Handle(t *testing.T) {
	cases := []struct {
		name    string
		tenant  bool
		detail  bool
		prepare func(*testing.T, *profileQueryFixture)
		status  int
		calls   int
		message string
	}{
		{
			name:   "provider omitted filters remain omitted",
			tenant: false,
			detail: false,
			status: http.StatusOK,
			calls:  2,
		},
		{
			name:   "provider forwards explicit false and zero",
			tenant: false,
			detail: false,
			status: http.StatusOK,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("internal", "false")
				f.query.Set("minimumAccessTier", "0")
				f.filter.Internal = cutil.GetPtr(false)
				f.filter.MinimumAccessTier = cutil.GetPtr(uint32(0))
			},
		},
		{
			name:   "provider forwards true and maximum tier",
			tenant: false,
			detail: false,
			status: http.StatusOK,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("internal", "true")
				f.query.Set("minimumAccessTier", "4294967295")
				f.filter.Internal = cutil.GetPtr(true)
				f.filter.MinimumAccessTier = cutil.GetPtr(uint32(4294967295))
				f.profiles.Profiles[0].Config.Internal = cutil.GetPtr(true)
				f.profiles.Profiles[0].Config.AccessTier = cutil.GetPtr(uint32(4294967295))
			},
		},
		{
			name:   "provider viewer can read",
			tenant: false,
			detail: true,
			status: http.StatusOK,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, []string{authz.ProviderViewerRole})
			},
		},
		{
			name:   "tenant omitted minimum uses default",
			tenant: true,
			detail: false,
			status: http.StatusOK,
			calls:  4,
		},
		{
			name:   "tenant equal minimum allowed",
			tenant: true,
			detail: false,
			status: http.StatusOK,
			calls:  4,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("minimumAccessTier", "2")
			},
		},
		{
			name:   "tenant higher minimum preserved",
			tenant: true,
			detail: false,
			status: http.StatusOK,
			calls:  4,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("minimumAccessTier", "3")
				f.filter.MinimumAccessTier = cutil.GetPtr(uint32(3))
			},
		},
		{
			name:    "tenant lower minimum rejected before listing",
			tenant:  true,
			detail:  false,
			status:  http.StatusForbidden,
			calls:   2,
			message: "access tiers lower",
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("minimumAccessTier", "1")
			},
		},
		{
			name:   "unset default tier is zero",
			tenant: true,
			detail: false,
			status: http.StatusOK,
			calls:  4,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.defaultProfile.Profiles[0].Config.AccessTier = nil
				f.query.Set("minimumAccessTier", "0")
				f.filter.MinimumAccessTier = cutil.GetPtr(uint32(0))
			},
		},
		{
			name:   "unprivileged tenant denied list",
			tenant: true,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.setPrivilege(t, false)
			},
		},
		{
			name:   "unprivileged tenant denied detail",
			tenant: true,
			detail: true,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.setPrivilege(t, false)
			},
		},
		{
			name:   "site denial overrides account grant",
			tenant: true,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				cdbm.TestBuildTenantSite(t, f.db, f.tenant, f.site, &cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)}, f.user)
			},
		},
		{
			name:   "site grant allows read",
			tenant: true,
			detail: false,
			status: http.StatusOK,
			calls:  4,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.setPrivilege(t, false)
				cdbm.TestBuildTenantSite(t, f.db, f.tenant, f.site, &cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)}, f.user)
			},
		},
		{
			name:   "foreign provider privilege insufficient",
			tenant: true,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				other := common.TestBuildInfrastructureProvider(t, f.db, "Other", "other-provider", f.user)
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("infrastructure_provider_id = ?", other.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
		},
		{
			name:   "pending account denied",
			tenant: true,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				_, err := f.db.DB.NewUpdate().Model(f.account).Set("status = ?", cdbm.TenantAccountStatusInvited).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
		},
		{
			name:   "provider cannot read another providers site",
			tenant: false,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				other := common.TestBuildInfrastructureProvider(t, f.db, "Other", "other-provider", f.user)
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("infrastructure_provider_id = ?", other.ID).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
		},
		{
			name:   "missing user",
			tenant: false,
			detail: false,
			status: http.StatusInternalServerError,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.user = nil
			},
		},
		{
			name:   "wrong org",
			tenant: false,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.org = "another-org"
			},
		},
		{
			name:   "wrong role",
			tenant: false,
			detail: false,
			status: http.StatusForbidden,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, nil)
			},
		},
		{
			name:   "missing site",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Del("siteId")
			},
		},
		{
			name:   "malformed site",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("siteId", "invalid")
			},
		},
		{
			name:   "absent site",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("siteId", uuid.NewString())
			},
		},
		{
			name:   "unknown filter",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("tenantId", uuid.NewString())
			},
		},
		{
			name:   "negative tier",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("minimumAccessTier", "-1")
			},
		},
		{
			name:   "overflow tier",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("minimumAccessTier", "4294967296")
			},
		},
		{
			name:   "invalid boolean",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("internal", "not-bool")
			},
		},
		{
			name:   "bad page size",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("pageSize", "101")
			},
		},
		{
			name:   "unsupported ordering",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("orderBy", "ACCESS_TIER_ASC")
			},
		},
		{
			name:   "unregistered site",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				_, err := f.db.DB.NewUpdate().Model(f.site).Set("status = ?", cdbm.SiteStatusPending).WherePK().Exec(context.Background())
				require.NoError(t, err)
			},
		},
		{
			name:   "native networking required",
			tenant: false,
			detail: false,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				_, err := cdbm.NewSiteDAO(f.db).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID,
					Config: &cdbm.SiteConfigUpdateInput{NativeNetworking: cutil.GetPtr(false)}})
				require.NoError(t, err)
			},
		},
		{
			name:   "missing tenant on site",
			tenant: true,
			detail: false,
			status: http.StatusNotFound,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.current.Tenant = nil
			},
		},
		{
			name:   "mismatched tenant fails closed",
			tenant: true,
			detail: false,
			status: http.StatusInternalServerError,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.current.Tenant.OrganizationId = "other-tenant"
			},
		},
		{
			name:   "missing default selection fails closed",
			tenant: true,
			detail: false,
			status: http.StatusInternalServerError,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.current.Tenant.RoutingProfileType = nil
			},
		},
		{
			name:   "missing default definition fails closed",
			tenant: true,
			detail: false,
			status: http.StatusInternalServerError,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.defaultProfile.Profiles = nil
			},
		},
		{
			name:   "missing default config fails closed",
			tenant: true,
			detail: false,
			status: http.StatusInternalServerError,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.defaultProfile.Profiles[0].Config = nil
			},
		},
		{
			name:   "name query error propagates",
			tenant: false,
			detail: false,
			status: http.StatusServiceUnavailable,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.namesErr = tp.NewNonRetryableApplicationError("Core unavailable", swe.ErrTypeNICoUnavailable, nil)
			},
		},
		{
			name:   "definition query error propagates",
			tenant: false,
			detail: true,
			status: http.StatusServiceUnavailable,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profilesErr = tp.NewNonRetryableApplicationError("Core unavailable", swe.ErrTypeNICoUnavailable, nil)
			},
		},
		{
			name:   "empty list skips definition RPC",
			tenant: false,
			detail: false,
			status: http.StatusOK,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.names.Names = nil
				f.expectedNames = nil
			},
		},
		{
			name:   "page beyond end skips definition RPC",
			tenant: false,
			detail: false,
			status: http.StatusOK,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("pageNumber", "2")
				f.expectedNames = nil
			},
		},
		{
			name:   "sorted descending page fetches only selected definitions",
			tenant: false,
			detail: false,
			status: http.StatusOK,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("pageSize", "1")
				f.query.Set("pageNumber", "2")
				f.query.Set("orderBy", "NAME_DESC")
				f.names.Names = []string{"Z", "EDGE", "A"}
			},
		},
		{
			name:   "lower tier after discovery cannot leak",
			tenant: true,
			detail: false,
			status: http.StatusConflict,
			calls:  4,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles[0].Config.AccessTier = cutil.GetPtr(uint32(1))
			},
		},
		{
			name:   "profile removed after discovery fails",
			tenant: false,
			detail: false,
			status: http.StatusConflict,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles = nil
			},
		},
		{
			name:   "unexpected profile cannot leak",
			tenant: false,
			detail: false,
			status: http.StatusInternalServerError,
			calls:  2,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles[0].Name = "OTHER"
			},
		},
		{
			name:   "tenant reads equal tier",
			tenant: true,
			detail: true,
			status: http.StatusOK,
			calls:  3,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles[0].Config.AccessTier = cutil.GetPtr(uint32(2))
			},
		},
		{
			name:   "tenant reads higher tier",
			tenant: true,
			detail: true,
			status: http.StatusOK,
			calls:  3,
		},
		{
			name:    "tenant lower tier denied",
			tenant:  true,
			detail:  true,
			status:  http.StatusForbidden,
			calls:   3,
			message: "access tier lower",
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles[0].Config.AccessTier = cutil.GetPtr(uint32(1))
			},
		},
		{
			name:   "tenant unset target tier is zero",
			tenant: true,
			detail: true,
			status: http.StatusForbidden,
			calls:  3,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles[0].Config.AccessTier = nil
			},
		},
		{
			name:   "provider can read tier zero",
			tenant: false,
			detail: true,
			status: http.StatusOK,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles[0].Config.AccessTier = nil
			},
		},
		{
			name:   "exact escaped profile name",
			tenant: false,
			detail: true,
			status: http.StatusOK,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profileName = "edge+%/租户"
				f.expectedNames = []string{f.profileName}
				f.profiles.Profiles[0].Name = f.profileName
			},
		},
		{
			name:   "invalid UTF-8 profile name is a client error",
			detail: true,
			status: http.StatusBadRequest,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profileName = string([]byte{0xff})
			},
		},
		{
			name:   "missing named profile",
			tenant: false,
			detail: true,
			status: http.StatusNotFound,
			calls:  1,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.profiles.Profiles = nil
			},
		},
		{
			name:   "detail rejects list filters",
			tenant: false,
			detail: true,
			status: http.StatusBadRequest,
			calls:  0,
			prepare: func(t *testing.T, f *profileQueryFixture) {
				f.query.Set("minimumAccessTier", "5")
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newProfileQueryFixture(t, tc.tenant)
			if tc.prepare != nil {
				tc.prepare(t, f)
			}
			var methods []string
			var requests []proto.Message
			var responses []proto.Message
			var errors []error
			if tc.tenant {
				methods = append(methods, corev1.Forge_FindTenant_FullMethodName, corev1.Forge_FindRoutingProfilesByNames_FullMethodName)
				requests = append(requests, &corev1.FindTenantRequest{TenantOrganizationId: f.tenant.Org}, &corev1.RoutingProfilesByNamesRequest{Names: []string{"EXTERNAL"}})
				responses = append(responses, f.current, f.defaultProfile)
				errors = append(errors, f.readErr, nil)
			}
			if !tc.detail {
				methods = append(methods, corev1.Forge_FindRoutingProfileNames_FullMethodName)
				requests = append(requests, &f.filter)
				responses = append(responses, &f.names)
				errors = append(errors, f.namesErr)
			}
			methods = append(methods, corev1.Forge_FindRoutingProfilesByNames_FullMethodName)
			requests = append(requests, &corev1.RoutingProfilesByNamesRequest{Names: f.expectedNames})
			responses = append(responses, f.profiles)
			errors = append(errors, f.profilesErr)
			for i := range tc.calls {
				f.expectProxy(t, methods[i], responses[i], errors[i])
			}
			path := "/org/:orgName/nico/vpc/routing-profile"
			target := "/org/" + f.org + "/nico/vpc/routing-profile"
			if tc.detail {
				path += "/:profileName"
				target += "/" + url.PathEscape(f.profileName)
			}
			e := echo.New()
			h := NewGetVPCRoutingProfileDefinitionHandler(f.db, f.scp)
			e.GET(path, h.Handle, func(next echo.HandlerFunc) echo.HandlerFunc {
				return func(c echo.Context) error {
					c.Set("user", f.user)
					return next(c)
				}
			})
			rec := httptest.NewRecorder()
			e.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, target+"?"+f.query.Encode(), nil))
			require.Equal(t, tc.status, rec.Code, rec.Body.String())
			if tc.message != "" {
				assert.Contains(t, rec.Body.String(), tc.message)
			}
			require.Len(t, f.calls, tc.calls)
			f.client.AssertExpectations(t)
			for i, call := range f.calls {
				got := requests[i].ProtoReflect().New().Interface()
				require.NoError(t, protojson.Unmarshal(call.RequestJSON, got))
				assert.True(t, proto.Equal(requests[i], got), "call %d: want %v, got %v", i, requests[i], got)
				assert.Empty(t, call.EncryptedSecrets)
			}
			if tc.status == http.StatusOK {
				var payload any
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &payload))
				if tc.detail {
					item := payload.(map[string]any)
					assert.Equal(t, f.profileName, item["name"])
					assert.Contains(t, item["config"], "routeTargetImports")
					assert.Empty(t, rec.Header().Get(pagination.ResponseHeaderName))
				} else {
					items := payload.([]any)
					require.Len(t, items, len(f.expectedNames))
					for i, name := range f.expectedNames {
						item := items[i].(map[string]any)
						assert.Equal(t, name, item["name"])
						assert.Contains(t, item["config"], "accessTier")
					}
					var page pagination.PageResponse
					require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &page))
					assert.Equal(t, len(f.names.Names), page.Total)
					if f.query.Get("pageNumber") == "2" {
						assert.Equal(t, 2, page.PageNumber)
					} else {
						assert.Equal(t, 1, page.PageNumber)
					}
					if f.query.Get("pageSize") == "1" {
						assert.Equal(t, 1, page.PageSize)
						assert.Equal(t, "NAME_DESC", *page.OrderBy)
					} else {
						assert.Equal(t, 20, page.PageSize)
						assert.Equal(t, "NAME_ASC", *page.OrderBy)
					}
				}
			}
		})
	}
}

type profileQueryFixture struct {
	*tenantRoutingProfileFixture
	profileName    string
	query          url.Values
	filter         corev1.RoutingProfileSearchFilter
	names          corev1.RoutingProfileNameList
	expectedNames  []string
	defaultProfile *corev1.NamedRoutingProfileList
	profiles       *corev1.NamedRoutingProfileList
	namesErr       error
	profilesErr    error
}

func newProfileQueryFixture(t *testing.T, tenant bool) *profileQueryFixture {
	t.Helper()
	base := newTenantRoutingProfileFixture(t)
	f := &profileQueryFixture{
		tenantRoutingProfileFixture: base,
		profileName:                 "EDGE",
		query:                       url.Values{"siteId": []string{base.siteID}},
		names:                       corev1.RoutingProfileNameList{Names: []string{"EDGE"}},
		expectedNames:               []string{"EDGE"},
		defaultProfile: &corev1.NamedRoutingProfileList{Profiles: []*corev1.NamedRoutingProfile{{
			Name:   "EXTERNAL",
			Config: &corev1.NamedRoutingProfileConfig{AccessTier: cutil.GetPtr(uint32(2))},
		}}},
		profiles: &corev1.NamedRoutingProfileList{Profiles: []*corev1.NamedRoutingProfile{{
			Name:   "EDGE",
			Config: &corev1.NamedRoutingProfileConfig{AccessTier: cutil.GetPtr(uint32(3))},
		}}},
	}
	if tenant {
		f.org = f.tenant.Org
		f.user = common.TestBuildUser(t, f.db, uuid.NewString(), f.org, []string{authz.TenantAdminRole})
		f.setPrivilege(t, true)
		f.filter.MinimumAccessTier = cutil.GetPtr(uint32(2))
	}
	return f
}

func TestFindNamedRoutingProfiles(t *testing.T) {
	cases := []struct {
		name      string
		names     []string
		errorType string
		status    int
		split     bool
	}{
		{
			name:      "configured limit smaller than REST page",
			names:     []string{"A", "B"},
			errorType: swe.ErrTypeNICoInvalidArgument,
			split:     true,
		},
		{
			name:      "single-name validation error is preserved",
			names:     []string{"A"},
			errorType: swe.ErrTypeNICoInvalidArgument,
			status:    http.StatusBadRequest,
		},
		{
			name:      "unavailable does not trigger more reads",
			names:     []string{"A", "B"},
			errorType: swe.ErrTypeNICoUnavailable,
			status:    http.StatusServiceUnavailable,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := &tenantRoutingProfileFixture{client: &tmocks.Client{}}
			method := corev1.Forge_FindRoutingProfilesByNames_FullMethodName
			f.expectProxy(t, method, &corev1.NamedRoutingProfileList{}, tp.NewNonRetryableApplicationError("query rejected", tc.errorType, nil))
			if tc.split {
				for _, name := range tc.names {
					f.expectProxy(t, method, &corev1.NamedRoutingProfileList{Profiles: []*corev1.NamedRoutingProfile{{
						Name:   name,
						Config: &corev1.NamedRoutingProfileConfig{},
					}}}, nil)
				}
			}
			profiles, apiErr := findNamedRoutingProfiles(context.Background(), f.client, tc.names)
			f.client.AssertExpectations(t)
			if tc.status != 0 {
				require.NotNil(t, apiErr)
				assert.Equal(t, tc.status, apiErr.Code)
				assert.Len(t, f.calls, 1)
				assert.Nil(t, profiles)
				return
			}
			require.Nil(t, apiErr)
			assert.Len(t, profiles, 2)
			require.Len(t, f.calls, 3)
			for i, want := range [][]string{{"A", "B"}, {"A"}, {"B"}} {
				var request corev1.RoutingProfilesByNamesRequest
				require.NoError(t, protojson.Unmarshal(f.calls[i].RequestJSON, &request))
				assert.Equal(t, want, request.Names)
			}
		})
	}
}
