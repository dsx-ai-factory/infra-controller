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
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
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
			status: http.StatusNotFound,
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
	})
	t.Run("authorization", func(t *testing.T) {
		assertVPCRoutingProfileProviderOnly(t, http.MethodPost, `{"ifVersionMatch":"V8-T1761856992374053","expectedInactiveVni":10100}`)
	})
	cases := []struct {
		name             string
		body             string
		getErr           error
		changeAck        func(*corev1.VpcReleaseInactiveVniResult)
		status           int
		calls            int
		mayHaveCommitted bool
	}{
		{name: "missing observed version cannot release", body: `{"expectedInactiveVni":10100}`, status: http.StatusBadRequest},
		{name: "stale version is returned without refresh or retry", getErr: tp.NewNonRetryableApplicationError("stale VPC version", swe.ErrTypeNICoFailedPrecondition, nil), status: http.StatusPreconditionFailed, calls: 1},
		{name: "ambiguous timeout is returned without retry", getErr: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), status: http.StatusGatewayTimeout, calls: 1, mayHaveCommitted: true},
		{name: "wrong released VNI is not acknowledged", changeAck: func(r *corev1.VpcReleaseInactiveVniResult) { r.ReleasedInactiveVni = 10101 }, status: http.StatusInternalServerError, calls: 1, mayHaveCommitted: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newVPCRoutingProfileHandlerFixture(t)
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
			rec := f.request(t, context.Background(), http.MethodPost, body)
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
	c := echo.New().NewContext(req, rec)
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
