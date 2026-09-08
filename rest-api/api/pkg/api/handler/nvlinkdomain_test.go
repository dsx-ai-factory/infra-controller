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
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel/trace"
	"go.opentelemetry.io/otel/trace/noop"
	temporalEnums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

type nvLinkDomainHandlerTestFixture struct {
	e            *echo.Echo
	dbSession    *cdb.Session
	scp          *sc.ClientPool
	org          string
	site         *cdbm.Site
	providerUser *cdbm.User
	tenantUser   *cdbm.User
	tracer       trace.Tracer
}

func newNVLinkDomainHandlerTestFixture(t *testing.T) *nvLinkDomainHandlerTestFixture {
	t.Helper()

	dbSession := testRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	return &nvLinkDomainHandlerTestFixture{
		e:            echo.New(),
		dbSession:    dbSession,
		scp:          scp,
		org:          org,
		site:         site,
		providerUser: testRackBuildUser(t, dbSession, "provider-user-domain", org, []string{authz.ProviderAdminRole}),
		tenantUser:   testRackBuildUser(t, dbSession, "tenant-user-domain", org, []string{authz.TenantAdminRole}),
		tracer:       noop.NewTracerProvider().Tracer("test"),
	}
}

type capturedNVLinkDomainProxyCall struct {
	options tclient.StartWorkflowOptions
	request grpcproxy.Request
}

func (f *nvLinkDomainHandlerTestFixture) installFlowReply(
	t *testing.T,
	reply *flowv1.SubmitTaskResponse,
) *capturedNVLinkDomainProxyCall {
	t.Helper()

	workflowRun := &tmocks.WorkflowRun{}
	testFlowProxyReply(t, workflowRun, reply)
	return f.installFlowRun(t, workflowRun)
}

func (f *nvLinkDomainHandlerTestFixture) installFlowError(
	t *testing.T,
	err error,
) *capturedNVLinkDomainProxyCall {
	t.Helper()

	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(err)
	return f.installFlowRun(t, workflowRun)
}

func (f *nvLinkDomainHandlerTestFixture) installFlowRun(
	t *testing.T,
	workflowRun *tmocks.WorkflowRun,
) *capturedNVLinkDomainProxyCall {
	t.Helper()

	captured := &capturedNVLinkDomainProxyCall{}
	temporalClient := &tmocks.Client{}
	temporalClient.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		mock.Anything,
		mock.Anything,
	).Run(func(args mock.Arguments) {
		captured.options = args.Get(1).(tclient.StartWorkflowOptions)
		assert.Equal(t, grpcproxy.Flow.WorkflowName, args.Get(2))
		captured.request = args.Get(3).(grpcproxy.Request)
	}).Return(workflowRun, nil)
	f.scp.IDClientMap[f.site.ID.String()] = temporalClient

	return captured
}

func (f *nvLinkDomainHandlerTestFixture) echoContext(
	t *testing.T,
	path string,
	body string,
	user *cdbm.User,
	nvLinkDomainID string,
) (echo.Context, *httptest.ResponseRecorder) {
	t.Helper()

	req := httptest.NewRequest(http.MethodPatch, path, strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := f.e.NewContext(req, rec)
	if nvLinkDomainID == "" {
		ec.SetParamNames("orgName")
		ec.SetParamValues(f.org)
	} else {
		ec.SetParamNames("orgName", "id")
		ec.SetParamValues(f.org, nvLinkDomainID)
	}
	ec.Set("user", user)
	ctx := context.WithValue(context.Background(), otelecho.TracerKey, f.tracer) //nolint:staticcheck // Middleware owns the context key.
	ec.SetRequest(ec.Request().WithContext(ctx))

	return ec, rec
}

func (f *nvLinkDomainHandlerTestFixture) setSiteStatus(t *testing.T, status string) {
	t.Helper()

	previousStatus := f.site.Status
	f.site.Status = status
	_, err := f.dbSession.DB.NewUpdate().Model(f.site).Column("status").WherePK().Exec(context.Background())
	require.NoError(t, err)
	t.Cleanup(func() {
		f.site.Status = previousStatus
		_, err := f.dbSession.DB.NewUpdate().Model(f.site).Column("status").WherePK().Exec(context.Background())
		require.NoError(t, err)
	})
}

func assertProxiedNVLinkDomainIDs(
	t *testing.T,
	requestJSON []byte,
	request proto.Message,
	wantNVLinkDomainIDs []string,
) {
	t.Helper()
	require.NoError(t, protojson.Unmarshal(requestJSON, request))

	var targetSpec *flowv1.OperationTargetSpec
	switch typed := request.(type) {
	case *flowv1.PowerOnRackRequest:
		targetSpec = typed.GetTargetSpec()
	case *flowv1.UpgradeFirmwareRequest:
		targetSpec = typed.GetTargetSpec()
	default:
		t.Fatalf("unsupported request type %T", request)
	}

	targets := targetSpec.GetNvlDomains().GetTargets()
	require.Len(t, targets, len(wantNVLinkDomainIDs))
	for i, nvLinkDomainID := range wantNVLinkDomainIDs {
		assert.Equal(t, nvLinkDomainID, targets[i].GetId().GetId())
	}
}

func assertValidationResponseData(
	t *testing.T,
	recorder *httptest.ResponseRecorder,
	wantKeys []string,
	wantBodySubstring string,
) {
	t.Helper()

	var response struct {
		Data map[string]json.RawMessage `json:"data"`
	}
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
	for _, key := range wantKeys {
		assert.Contains(t, response.Data, key)
	}
	if wantBodySubstring != "" {
		assert.Contains(t, recorder.Body.String(), wantBodySubstring)
	}
}

func TestUpdateNVLinkDomainPowerStateHandler_Handle(t *testing.T) {
	fixture := newNVLinkDomainHandlerTestFixture(t)
	handler := NewUpdateNVLinkDomainPowerStateHandler(fixture.dbSession, fixture.scp)
	nvLinkDomainID := uuid.NewString()
	ruleID := uuid.NewString()

	tests := []struct {
		name           string
		nvLinkDomainID string
		body           string
		user           *cdbm.User
		siteStatus     string
		wantStatus     int
		wantProxy      bool
		flowError      bool
		wantBody       string
	}{
		{
			name:           "proxies domain target through shared Flow workflow",
			nvLinkDomainID: strings.ToUpper(nvLinkDomainID),
			body:           fmt.Sprintf(`{"siteId":%q,"state":"on","ruleId":%q,"overrideReadinessCheck":true}`, strings.ToUpper(fixture.site.ID.String()), ruleID),
			user:           fixture.providerUser,
			wantStatus:     http.StatusOK,
			wantProxy:      true,
		},
		{
			name:           "preserves Flow proxy error response",
			nvLinkDomainID: nvLinkDomainID,
			body:           fmt.Sprintf(`{"siteId":%q,"state":"on"}`, fixture.site.ID.String()),
			user:           fixture.providerUser,
			wantStatus:     http.StatusInternalServerError,
			wantProxy:      true,
			flowError:      true,
		},
		{
			name:           "rejects malformed domain ID",
			nvLinkDomainID: "not-a-uuid",
			body:           fmt.Sprintf(`{"siteId":%q,"state":"on"}`, fixture.site.ID.String()),
			user:           fixture.providerUser,
			wantStatus:     http.StatusBadRequest,
		},
		{
			name:           "authorizes before validating the request",
			nvLinkDomainID: "not-a-uuid",
			body:           `{}`,
			user:           fixture.tenantUser,
			wantStatus:     http.StatusForbidden,
		},
		{
			name:           "rejects a site that is not registered",
			nvLinkDomainID: nvLinkDomainID,
			body:           fmt.Sprintf(`{"siteId":%q,"state":"on"}`, fixture.site.ID.String()),
			user:           fixture.providerUser,
			siteStatus:     cdbm.SiteStatusPending,
			wantStatus:     http.StatusBadRequest,
			wantBody:       "Site is not in Registered state",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.siteStatus != "" {
				fixture.setSiteStatus(t, test.siteStatus)
			}
			var captured *capturedNVLinkDomainProxyCall
			if test.flowError {
				captured = fixture.installFlowError(t, fmt.Errorf("domain not found"))
			} else {
				captured = fixture.installFlowReply(t, &flowv1.SubmitTaskResponse{
					TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
				})
			}
			path := fmt.Sprintf("/v2/org/%s/nico/domain/nvlink/%s/power", fixture.org, test.nvLinkDomainID)
			ec, rec := fixture.echoContext(t, path, test.body, test.user, test.nvLinkDomainID)

			err := handler.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, test.wantStatus, rec.Code, rec.Body.String())
			if test.wantBody != "" {
				assert.Contains(t, rec.Body.String(), test.wantBody)
			}
			if !test.wantProxy {
				assert.Empty(t, captured.request.FullMethod)
				return
			}

			assert.Equal(t, flowv1.Flow_PowerOnRack_FullMethodName, captured.request.FullMethod)
			assert.True(t, strings.HasPrefix(captured.options.ID, "nvlink-domain-power-state-update-on-"))
			assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, captured.options.WorkflowIDConflictPolicy)
			flowRequest := &flowv1.PowerOnRackRequest{}
			assertProxiedNVLinkDomainIDs(t, captured.request.RequestJSON, flowRequest, []string{nvLinkDomainID})
			if test.flowError {
				var response map[string]any
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, "domain not found", response["message"])
				return
			}
			assert.Equal(t, ruleID, flowRequest.GetRuleId().GetId())
			assert.True(t, flowRequest.GetOverrideReadinessCheck())

			var response model.APIUpdatePowerStateResponse
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
			require.Len(t, response.TaskIDs, 1)
		})
	}
}

func TestBatchUpdateNVLinkDomainPowerStateHandler_Handle(t *testing.T) {
	fixture := newNVLinkDomainHandlerTestFixture(t)
	handler := NewBatchUpdateNVLinkDomainPowerStateHandler(fixture.dbSession, fixture.scp)
	nvLinkDomainIDs := []string{
		"FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
		"00000000000000000000000000000001",
	}
	canonicalNVLinkDomainIDs := []string{
		"00000000-0000-0000-0000-000000000001",
		"ffffffff-ffff-ffff-ffff-ffffffffffff",
	}

	tests := []struct {
		name       string
		body       string
		user       *cdbm.User
		siteStatus string
		wantStatus int
		wantProxy  bool
		wantData   []string
		wantBody   string
	}{
		{
			name: "proxies all domain targets through shared Flow workflow",
			body: fmt.Sprintf(
				`{"siteId":%q,"domainIds":[%q,%q],"state":"on"}`,
				fixture.site.ID.String(),
				nvLinkDomainIDs[0],
				nvLinkDomainIDs[1],
			),
			wantStatus: http.StatusOK,
			wantProxy:  true,
		},
		{
			name:       "rejects empty domain list",
			body:       fmt.Sprintf(`{"siteId":%q,"domainIds":[],"state":"on"}`, fixture.site.ID.String()),
			wantStatus: http.StatusBadRequest,
			wantData:   []string{"domainIds"},
		},
		{
			name:       "rejects duplicate domains",
			body:       fmt.Sprintf(`{"siteId":%q,"domainIds":[%q,%q],"state":"on"}`, fixture.site.ID.String(), nvLinkDomainIDs[0], nvLinkDomainIDs[0]),
			wantStatus: http.StatusBadRequest,
			wantData:   []string{"domainIds"},
			wantBody:   "duplicates NVLink Domain ID",
		},
		{
			name:       "returns all field validation errors",
			body:       `{"domainIds":["bad"]}`,
			wantStatus: http.StatusBadRequest,
			wantData:   []string{"siteId", "domainIds", "state"},
			wantBody:   "NVLink Domain ID must be a non-zero UUID",
		},
		{
			name:       "authorizes before validating the request",
			body:       `{"domainIds":["bad"]}`,
			user:       fixture.tenantUser,
			wantStatus: http.StatusForbidden,
		},
		{
			name:       "rejects a site that is not registered",
			body:       fmt.Sprintf(`{"siteId":%q,"domainIds":[%q],"state":"on"}`, fixture.site.ID.String(), nvLinkDomainIDs[0]),
			siteStatus: cdbm.SiteStatusPending,
			wantStatus: http.StatusBadRequest,
			wantBody:   "Site is not in Registered state",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.siteStatus != "" {
				fixture.setSiteStatus(t, test.siteStatus)
			}
			user := test.user
			if user == nil {
				user = fixture.providerUser
			}
			captured := fixture.installFlowReply(t, &flowv1.SubmitTaskResponse{})
			path := fmt.Sprintf("/v2/org/%s/nico/domain/nvlink/power", fixture.org)
			ec, rec := fixture.echoContext(t, path, test.body, user, "")

			err := handler.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, test.wantStatus, rec.Code, rec.Body.String())
			if !test.wantProxy {
				assert.Empty(t, captured.request.FullMethod)
				if len(test.wantData) > 0 {
					assertValidationResponseData(t, rec, test.wantData, test.wantBody)
				} else if test.wantBody != "" {
					assert.Contains(t, rec.Body.String(), test.wantBody)
				}
				return
			}

			assert.Equal(t, flowv1.Flow_PowerOnRack_FullMethodName, captured.request.FullMethod)
			assertProxiedNVLinkDomainIDs(t, captured.request.RequestJSON, &flowv1.PowerOnRackRequest{}, canonicalNVLinkDomainIDs)
		})
	}
}

func TestUpdateNVLinkDomainFirmwareHandler_Handle(t *testing.T) {
	fixture := newNVLinkDomainHandlerTestFixture(t)
	handler := NewUpdateNVLinkDomainFirmwareHandler(fixture.dbSession, fixture.scp)
	nvLinkDomainID := uuid.NewString()
	version := "1.2.3"
	ruleID := uuid.NewString()

	tests := []struct {
		name             string
		nvLinkDomainID   string
		body             string
		user             *cdbm.User
		siteStatus       string
		wantStatus       int
		wantProxy        bool
		wantVersion      *string
		wantVersionUnset bool
		wantBody         string
	}{
		{
			name:        "proxies domain target through shared Flow workflow",
			body:        fmt.Sprintf(`{"siteId":%q,"version":%q,"ruleId":%q,"overrideReadinessCheck":true}`, fixture.site.ID.String(), version, ruleID),
			wantStatus:  http.StatusOK,
			wantProxy:   true,
			wantVersion: &version,
		},
		{
			name:             "normalizes an empty version to unset",
			body:             fmt.Sprintf(`{"siteId":%q,"version":""}`, fixture.site.ID.String()),
			wantStatus:       http.StatusOK,
			wantProxy:        true,
			wantVersionUnset: true,
		},
		{
			name:       "rejects missing site",
			body:       `{}`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:           "authorizes before validating the request",
			nvLinkDomainID: "not-a-uuid",
			body:           `{}`,
			user:           fixture.tenantUser,
			wantStatus:     http.StatusForbidden,
		},
		{
			name:       "rejects a site that is not registered",
			body:       fmt.Sprintf(`{"siteId":%q}`, fixture.site.ID.String()),
			siteStatus: cdbm.SiteStatusPending,
			wantStatus: http.StatusBadRequest,
			wantBody:   "Site is not in Registered state",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.siteStatus != "" {
				fixture.setSiteStatus(t, test.siteStatus)
			}
			user := test.user
			if user == nil {
				user = fixture.providerUser
			}
			testNVLinkDomainID := test.nvLinkDomainID
			if testNVLinkDomainID == "" {
				testNVLinkDomainID = nvLinkDomainID
			}
			captured := fixture.installFlowReply(t, &flowv1.SubmitTaskResponse{})
			path := fmt.Sprintf("/v2/org/%s/nico/domain/nvlink/%s/firmware", fixture.org, testNVLinkDomainID)
			ec, rec := fixture.echoContext(t, path, test.body, user, testNVLinkDomainID)

			err := handler.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, test.wantStatus, rec.Code, rec.Body.String())
			if test.wantBody != "" {
				assert.Contains(t, rec.Body.String(), test.wantBody)
			}
			if !test.wantProxy {
				assert.Empty(t, captured.request.FullMethod)
				return
			}

			assert.Equal(t, flowv1.Flow_UpgradeFirmware_FullMethodName, captured.request.FullMethod)
			request := &flowv1.UpgradeFirmwareRequest{}
			assertProxiedNVLinkDomainIDs(t, captured.request.RequestJSON, request, []string{nvLinkDomainID})
			if test.wantVersionUnset {
				assert.Nil(t, request.TargetVersion)
			} else {
				require.NotNil(t, request.TargetVersion)
				assert.Equal(t, *test.wantVersion, request.GetTargetVersion())
			}
			if test.wantVersion != nil {
				assert.Equal(t, ruleID, request.GetRuleId().GetId())
				assert.True(t, request.GetOverrideReadinessCheck())
			}
		})
	}
}

func TestBatchUpdateNVLinkDomainFirmwareHandler_Handle(t *testing.T) {
	fixture := newNVLinkDomainHandlerTestFixture(t)
	handler := NewBatchUpdateNVLinkDomainFirmwareHandler(fixture.dbSession, fixture.scp)
	nvLinkDomainIDs := []string{
		"FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
		"00000000000000000000000000000001",
	}
	canonicalNVLinkDomainIDs := []string{
		"00000000-0000-0000-0000-000000000001",
		"ffffffff-ffff-ffff-ffff-ffffffffffff",
	}

	tests := []struct {
		name             string
		body             string
		user             *cdbm.User
		siteStatus       string
		wantStatus       int
		wantProxy        bool
		wantVersion      string
		wantVersionUnset bool
		wantBody         string
	}{
		{
			name: "proxies all domain targets through shared Flow workflow",
			body: fmt.Sprintf(
				`{"siteId":%q,"domainIds":[%q,%q]}`,
				fixture.site.ID.String(),
				nvLinkDomainIDs[0],
				nvLinkDomainIDs[1],
			),
			wantStatus:       http.StatusOK,
			wantProxy:        true,
			wantVersionUnset: true,
		},
		{
			name: "normalizes an empty version to unset",
			body: fmt.Sprintf(
				`{"siteId":%q,"domainIds":[%q,%q],"version":""}`,
				fixture.site.ID.String(),
				nvLinkDomainIDs[0],
				nvLinkDomainIDs[1],
			),
			wantStatus:       http.StatusOK,
			wantProxy:        true,
			wantVersionUnset: true,
		},
		{
			name: "forwards the requested version",
			body: fmt.Sprintf(
				`{"siteId":%q,"domainIds":[%q,%q],"version":"1.2.3"}`,
				fixture.site.ID.String(),
				nvLinkDomainIDs[0],
				nvLinkDomainIDs[1],
			),
			wantStatus:  http.StatusOK,
			wantProxy:   true,
			wantVersion: "1.2.3",
		},
		{
			name:       "rejects malformed domain ID",
			body:       fmt.Sprintf(`{"siteId":%q,"domainIds":["bad"]}`, fixture.site.ID.String()),
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "authorizes before validating the request",
			body:       `{"domainIds":["bad"]}`,
			user:       fixture.tenantUser,
			wantStatus: http.StatusForbidden,
		},
		{
			name:       "rejects a site that is not registered",
			body:       fmt.Sprintf(`{"siteId":%q,"domainIds":[%q]}`, fixture.site.ID.String(), nvLinkDomainIDs[0]),
			siteStatus: cdbm.SiteStatusPending,
			wantStatus: http.StatusBadRequest,
			wantBody:   "Site is not in Registered state",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.siteStatus != "" {
				fixture.setSiteStatus(t, test.siteStatus)
			}
			user := test.user
			if user == nil {
				user = fixture.providerUser
			}
			captured := fixture.installFlowReply(t, &flowv1.SubmitTaskResponse{})
			path := fmt.Sprintf("/v2/org/%s/nico/domain/nvlink/firmware", fixture.org)
			ec, rec := fixture.echoContext(t, path, test.body, user, "")

			err := handler.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, test.wantStatus, rec.Code, rec.Body.String())
			if test.wantBody != "" {
				assert.Contains(t, rec.Body.String(), test.wantBody)
			}
			if !test.wantProxy {
				assert.Empty(t, captured.request.FullMethod)
				return
			}

			assert.Equal(t, flowv1.Flow_UpgradeFirmware_FullMethodName, captured.request.FullMethod)
			request := &flowv1.UpgradeFirmwareRequest{}
			assertProxiedNVLinkDomainIDs(t, captured.request.RequestJSON, request, canonicalNVLinkDomainIDs)
			if test.wantVersionUnset {
				assert.Nil(t, request.TargetVersion)
			} else {
				require.NotNil(t, request.TargetVersion)
				assert.Equal(t, test.wantVersion, request.GetTargetVersion())
			}
		})
	}
}

func TestNewNVLinkDomainOperationWorkflowIdentity(t *testing.T) {
	emptyRuleID := ""
	invalidRuleID := "bad"
	ruleID := "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"
	canonicalRuleID := "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"

	tests := []struct {
		name            string
		siteID          string
		nvLinkDomainIDs []string
		ruleID          *string
		want            nvLinkDomainOperationWorkflowIdentity
		wantErr         string
	}{
		{
			name:   "canonicalizes UUIDs and domain order",
			siteID: "BBBBBBBB-BBBB-BBBB-BBBB-BBBBBBBBBBBB",
			nvLinkDomainIDs: []string{
				"FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
				"00000000000000000000000000000001",
			},
			ruleID: &ruleID,
			want: nvLinkDomainOperationWorkflowIdentity{
				SiteID: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
				NVLinkDomainIDs: []string{
					"00000000-0000-0000-0000-000000000001",
					"ffffffff-ffff-ffff-ffff-ffffffffffff",
				},
				RuleID: &canonicalRuleID,
			},
		},
		{
			name:            "normalizes an empty rule ID to unset",
			siteID:          "CCCCCCCC-CCCC-CCCC-CCCC-CCCCCCCCCCCC",
			nvLinkDomainIDs: []string{"DDDDDDDD-DDDD-DDDD-DDDD-DDDDDDDDDDDD"},
			ruleID:          &emptyRuleID,
			want: nvLinkDomainOperationWorkflowIdentity{
				SiteID:          "cccccccc-cccc-cccc-cccc-cccccccccccc",
				NVLinkDomainIDs: []string{"dddddddd-dddd-dddd-dddd-dddddddddddd"},
			},
		},
		{
			name:            "rejects invalid site ID",
			siteID:          "bad",
			nvLinkDomainIDs: []string{uuid.NewString()},
			wantErr:         "site ID must be a UUID",
		},
		{
			name:            "rejects invalid NVLink Domain ID",
			siteID:          uuid.NewString(),
			nvLinkDomainIDs: []string{"bad"},
			wantErr:         "NVLink Domain ID at index 0 must be a non-zero UUID",
		},
		{
			name:            "rejects nil NVLink Domain ID",
			siteID:          uuid.NewString(),
			nvLinkDomainIDs: []string{uuid.NewString(), uuid.Nil.String()},
			wantErr:         "NVLink Domain ID at index 1 must be a non-zero UUID",
		},
		{
			name:            "rejects invalid rule ID",
			siteID:          uuid.NewString(),
			nvLinkDomainIDs: []string{uuid.NewString()},
			ruleID:          &invalidRuleID,
			wantErr:         "rule ID must be a UUID",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := newNVLinkDomainOperationWorkflowIdentity(test.siteID, test.nvLinkDomainIDs, test.ruleID)
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}

func TestNVLinkDomainOperationWorkflowIdentity_PowerWorkflowID(t *testing.T) {
	ruleID := uuid.NewString()
	identity := nvLinkDomainOperationWorkflowIdentity{
		SiteID:          uuid.NewString(),
		NVLinkDomainIDs: []string{uuid.NewString()},
		RuleID:          &ruleID,
	}
	baseID := identity.powerWorkflowID("forceoff", true)

	tests := []struct {
		name       string
		identity   nvLinkDomainOperationWorkflowIdentity
		state      string
		override   bool
		wantPrefix string
		wantBase   bool
	}{
		{
			name:       "is deterministic for the same operation",
			identity:   identity,
			state:      "forceoff",
			override:   true,
			wantPrefix: "nvlink-domain-power-state-update-forceoff-",
			wantBase:   true,
		},
		{
			name:       "includes a state change in the identity",
			identity:   identity,
			state:      "on",
			override:   true,
			wantPrefix: "nvlink-domain-power-state-update-on-",
		},
		{
			name: "includes a rule change in the identity",
			identity: nvLinkDomainOperationWorkflowIdentity{
				SiteID:          identity.SiteID,
				NVLinkDomainIDs: identity.NVLinkDomainIDs,
			},
			state:      "forceoff",
			override:   true,
			wantPrefix: "nvlink-domain-power-state-update-forceoff-",
		},
		{
			name:       "includes the readiness override in the identity",
			identity:   identity,
			state:      "forceoff",
			wantPrefix: "nvlink-domain-power-state-update-forceoff-",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := test.identity.powerWorkflowID(test.state, test.override)
			assert.True(t, strings.HasPrefix(got, test.wantPrefix))
			if test.wantBase {
				assert.Equal(t, baseID, got)
				return
			}
			assert.NotEqual(t, baseID, got)
		})
	}
}

func TestNVLinkDomainOperationWorkflowIdentity_FirmwareWorkflowID(t *testing.T) {
	version := "1.2.3"
	emptyVersion := ""
	ruleID := uuid.NewString()
	identity := nvLinkDomainOperationWorkflowIdentity{
		SiteID:          uuid.NewString(),
		NVLinkDomainIDs: []string{uuid.NewString()},
		RuleID:          &ruleID,
	}
	baseID := identity.firmwareWorkflowID(&version, true)
	unsetVersionID := identity.firmwareWorkflowID(nil, true)

	tests := []struct {
		name             string
		version          *string
		override         bool
		wantBase         bool
		wantUnsetVersion bool
	}{
		{
			name:     "is deterministic for the same operation",
			version:  &version,
			override: true,
			wantBase: true,
		},
		{
			name:     "includes a version change in the identity",
			override: true,
		},
		{
			name:             "normalizes an empty version to unset",
			version:          &emptyVersion,
			override:         true,
			wantUnsetVersion: true,
		},
		{
			name:    "includes the readiness override in the identity",
			version: &version,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := identity.firmwareWorkflowID(test.version, test.override)
			assert.True(t, strings.HasPrefix(got, "nvlink-domain-firmware-update-"))
			if test.wantBase {
				assert.Equal(t, baseID, got)
				return
			}
			if test.wantUnsetVersion {
				assert.Equal(t, unsetVersionID, got)
				return
			}
			assert.NotEqual(t, baseID, got)
		})
	}
}
