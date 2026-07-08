// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	grpccodes "google.golang.org/grpc/codes"
	grpcstatus "google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// countMockCalls returns the number of recorded invocations of method on the testify mock.
func countMockCalls(m *mock.Mock, method string) int {
	n := 0
	for _, c := range m.Calls {
		if c.Method == method {
			n++
		}
	}
	return n
}

// TestTenantIdentityWorkflowHandlers_TimeoutReturns500AndTerminatesWorkflow verifies the tenant-identity handlers that still use bespoke workflows return 500 and terminate their workflow when the underlying Temporal workflow times out.
func TestTenantIdentityWorkflowHandlers_TimeoutReturns500AndTerminatesWorkflow(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil)))

	const (
		tenantOrg   = "test-identity-tenant-org"
		providerOrg = "test-identity-provider-org"
	)
	tenantUser := testVPCBuildUser(t, dbSession, "test-identity-tenant-user", tenantOrg, []string{auth.TenantAdminRole})
	tenant := testVPCBuildTenant(t, dbSession, "test-identity-tenant", tenantOrg, tenantUser)
	providerUser := testVPCBuildUser(t, dbSession, "test-identity-provider-user", providerOrg, []string{auth.ProviderAdminRole})
	infraProvider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-identity-ip", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, infraProvider, "test-identity-site", false, false, cdbm.SiteStatusRegistered, providerUser)
	_ = testBuildAllocation(t, dbSession, site, tenant, "test-identity-alloc", tenantUser)

	testConfig := common.GetTestConfig()
	temporalCfg, _ := testConfig.GetTemporalConfig()
	temporalClient := &tmocks.Client{}
	siteClientPool := sc.NewClientPool(temporalCfg)
	siteClientPool.IDClientMap[site.ID.String()] = temporalClient

	timeoutRun := &tmocks.WorkflowRun{}
	timeoutRun.On("GetID").Return("test-identity-timeout-wf-id")
	timeoutRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))
	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("string"),
		mock.Anything,
	).Return(timeoutRun, nil)
	temporalClient.Mock.On("TerminateWorkflow",
		mock.Anything, mock.Anything, mock.Anything, mock.Anything,
	).Return(nil)

	echoSrv := echo.New()
	siteIDStr := site.ID.String()

	configBody, err := json.Marshal(model.APITenantIdentityConfigCreateOrUpdateRequest{
		Enabled:         cutil.GetPtr(true),
		DefaultAudience: "spiffe://test/aud",
		Issuer:          "https://issuer.test/{org}",
		TokenTtlSeconds: 3600,
	})
	require.NoError(t, err)
	tokenDelegationBody, err := json.Marshal(model.APITenantIdentityTokenDelegationCreateOrUpdateRequest{
		TokenEndpoint:        "https://callback.test/exchange",
		SubjectTokenAudience: "https://aud.test",
	})
	require.NoError(t, err)

	tests := []struct {
		name       string
		method     string
		body       []byte
		entity     string
		workflow   string
		user       *cdbm.User
		newHandler func() echo.HandlerFunc
	}{
		{
			name: "PUT tenant-identity/config", method: http.MethodPut, body: configBody,
			entity: "TenantIdentity", workflow: "CreateOrUpdateTenantIdentityConfiguration", user: tenantUser,
			newHandler: func() echo.HandlerFunc {
				return NewCreateOrUpdateTenantIdentityConfigHandler(dbSession, siteClientPool).Handle
			},
		},
		{
			name: "GET tenant-identity/config", method: http.MethodGet,
			entity: "TenantIdentity", workflow: "GetTenantIdentityConfiguration", user: tenantUser,
			newHandler: func() echo.HandlerFunc {
				return NewGetTenantIdentityConfigHandler(dbSession, siteClientPool).Handle
			},
		},
		{
			name: "DELETE tenant-identity/config", method: http.MethodDelete,
			entity: "TenantIdentity", workflow: "DeleteTenantIdentityConfiguration", user: tenantUser,
			newHandler: func() echo.HandlerFunc {
				return NewDeleteTenantIdentityConfigHandler(dbSession, siteClientPool).Handle
			},
		},
		{
			name: "PUT tenant-identity/token-delegation", method: http.MethodPut, body: tokenDelegationBody,
			entity: "TenantIdentityTokenDelegation", workflow: "CreateOrUpdateTenantIdentityTokenDelegation", user: tenantUser,
			newHandler: func() echo.HandlerFunc {
				return NewCreateOrUpdateTenantIdentityTokenDelegationHandler(dbSession, siteClientPool).Handle
			},
		},
		{
			name: "GET tenant-identity/token-delegation", method: http.MethodGet,
			entity: "TenantIdentityTokenDelegation", workflow: "GetTenantIdentityTokenDelegation", user: tenantUser,
			newHandler: func() echo.HandlerFunc {
				return NewGetTenantIdentityTokenDelegationHandler(dbSession, siteClientPool).Handle
			},
		},
		{
			name: "DELETE tenant-identity/token-delegation", method: http.MethodDelete,
			entity: "TenantIdentityTokenDelegation", workflow: "DeleteTenantIdentityTokenDelegation", user: tenantUser,
			newHandler: func() echo.HandlerFunc {
				return NewDeleteTenantIdentityTokenDelegationHandler(dbSession, siteClientPool).Handle
			},
		},
		{
			name: "GET .well-known/jwks.json (oidc)", method: http.MethodGet,
			entity: "TenantIdentity", workflow: "GetJWKS", user: nil,
			newHandler: func() echo.HandlerFunc {
				return NewGetJWKSHandler(dbSession, siteClientPool, corev1.JwksKind_Oidc).Handle
			},
		},
		{
			name: "GET .well-known/openid-configuration", method: http.MethodGet,
			entity: "TenantIdentity", workflow: "GetOpenIDConfiguration", user: nil,
			newHandler: func() echo.HandlerFunc {
				return NewGetOpenIDConfigurationHandler(dbSession, siteClientPool).Handle
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			beforeTerminate := countMockCalls(&temporalClient.Mock, "TerminateWorkflow")

			body := strings.NewReader("")
			if tt.body != nil {
				body = strings.NewReader(string(tt.body))
			}
			httpReq := httptest.NewRequest(tt.method, "/", body)
			httpReq.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			recorder := httptest.NewRecorder()
			echoCtx := echoSrv.NewContext(httpReq, recorder)
			echoCtx.SetParamNames("orgName", "siteID")
			echoCtx.SetParamValues(tenantOrg, siteIDStr)
			if tt.user != nil {
				echoCtx.Set("user", tt.user)
			}

			require.NoError(t, tt.newHandler()(echoCtx))
			require.Equal(t, http.StatusInternalServerError, recorder.Code, "body=%s", recorder.Body.String())
			expected := "Failed to perform " + tt.entity + " " + tt.workflow + " - timeout occurred executing workflow on Site"
			assert.Contains(t, recorder.Body.String(), expected)
			assert.Equal(t, beforeTerminate+1, countMockCalls(&temporalClient.Mock, "TerminateWorkflow"),
				"expected exactly one TerminateWorkflow call for %s", tt.workflow)
		})
	}
}

// TestReencryptTenantIdentitySecretsHandler_Handle verifies that only the re-encryption endpoint dispatches through the generic Core gRPC proxy and returns the curated REST response.
func TestReencryptTenantIdentitySecretsHandler_Handle(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil)))

	const (
		providerOrg        = "test-reencrypt-provider-org"
		tenantOrg          = "test-reencrypt-tenant-org"
		otherSiteTenantOrg = "test-reencrypt-other-site-tenant-org"
		unknownTenantOrg   = "test-reencrypt-unknown-tenant-org"
	)
	providerUser := testVPCBuildUser(t, dbSession, "test-reencrypt-provider-user", providerOrg, []string{auth.ProviderAdminRole})
	infraProvider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-reencrypt-ip", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, infraProvider, "test-reencrypt-site", false, false, cdbm.SiteStatusRegistered, providerUser)
	otherSite := testVPCBuildSite(t, dbSession, infraProvider, "test-reencrypt-other-site", false, false, cdbm.SiteStatusRegistered, providerUser)

	tenantUser := testVPCBuildUser(t, dbSession, "test-reencrypt-tenant-user", tenantOrg, []string{auth.TenantAdminRole})
	tenant := testVPCBuildTenant(t, dbSession, "test-reencrypt-tenant", tenantOrg, tenantUser)
	common.TestBuildTenantSite(t, dbSession, tenant, site, providerUser)

	otherSiteTenantUser := testVPCBuildUser(t, dbSession, "test-reencrypt-other-site-tenant-user", otherSiteTenantOrg, []string{auth.TenantAdminRole})
	otherSiteTenant := testVPCBuildTenant(t, dbSession, "test-reencrypt-other-site-tenant", otherSiteTenantOrg, otherSiteTenantUser)
	common.TestBuildTenantSite(t, dbSession, otherSiteTenant, otherSite, providerUser)

	responseJSON, err := protojson.Marshal(&corev1.ReencryptTenantIdentitySecretsResponse{
		RowsExamined:           3,
		RowsUpdated:            2,
		RowsSkippedAllOnTarget: 1,
		FieldsReencrypted:      4,
		FieldsSkippedOnTarget:  2,
		CurrentEncryptionKeyId: "key-2",
	})
	require.NoError(t, err)

	var proxiedRequest grpcproxy.Request
	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Return(nil).Run(func(args mock.Arguments) {
		response := args.Get(1).(*grpcproxy.Response)
		response.ResponseJSON = responseJSON
	})

	temporalClient := &tmocks.Client{}
	temporalClient.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(request grpcproxy.Request) bool {
			proxiedRequest = request
			return true
		}),
	).Return(workflowRun, nil).Twice()

	testConfig := common.GetTestConfig()
	temporalConfig, _ := testConfig.GetTemporalConfig()
	siteClientPool := sc.NewClientPool(temporalConfig)
	siteClientPool.IDClientMap[site.ID.String()] = temporalClient

	echoServer := echo.New()
	handler := NewReencryptTenantIdentitySecretsHandler(dbSession, siteClientPool)
	performRequest := func(apiRequest model.APIReencryptTenantIdentitySecretsRequest) (*httptest.ResponseRecorder, []byte) {
		requestBody, marshalErr := json.Marshal(apiRequest)
		require.NoError(t, marshalErr)

		httpRequest := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(requestBody)))
		httpRequest.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		recorder := httptest.NewRecorder()
		echoContext := echoServer.NewContext(httpRequest, recorder)
		echoContext.SetParamNames("orgName", "siteID")
		echoContext.SetParamValues(providerOrg, site.ID.String())
		echoContext.Set("user", providerUser)

		require.NoError(t, handler.Handle(echoContext))
		return recorder, requestBody
	}

	t.Run("omitted organization targets all organizations", func(t *testing.T) {
		recorder, requestBody := performRequest(model.APIReencryptTenantIdentitySecretsRequest{DryRun: true})
		assert.JSONEq(t, `{"dryRun":true}`, string(requestBody))
		require.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
		assert.Equal(t, corev1.Forge_ReencryptTenantIdentitySecrets_FullMethodName, proxiedRequest.FullMethod)

		var coreRequest corev1.ReencryptTenantIdentitySecretsRequest
		require.NoError(t, protojson.Unmarshal(proxiedRequest.RequestJSON, &coreRequest))
		assert.True(t, coreRequest.GetDryRun())
		assert.Nil(t, coreRequest.OrganizationId)

		var apiResponse model.APIReencryptTenantIdentitySecretsResponse
		require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &apiResponse))
		assert.Equal(t, 3, apiResponse.RowsExamined)
		assert.Equal(t, 2, apiResponse.RowsUpdated)
		assert.Equal(t, "key-2", apiResponse.CurrentEncryptionKeyID)
	})

	t.Run("tenant with allocation on selected site is forwarded", func(t *testing.T) {
		recorder, _ := performRequest(model.APIReencryptTenantIdentitySecretsRequest{
			OrganizationID: cutil.GetPtr(tenantOrg),
		})
		require.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())

		var coreRequest corev1.ReencryptTenantIdentitySecretsRequest
		require.NoError(t, protojson.Unmarshal(proxiedRequest.RequestJSON, &coreRequest))
		assert.Equal(t, tenantOrg, coreRequest.GetOrganizationId())
	})

	t.Run("unknown tenant organization is rejected before proxy dispatch", func(t *testing.T) {
		recorder, _ := performRequest(model.APIReencryptTenantIdentitySecretsRequest{
			OrganizationID: cutil.GetPtr(unknownTenantOrg),
		})
		assert.Equal(t, http.StatusBadRequest, recorder.Code)
		assert.Contains(t, recorder.Body.String(), "Could not find Tenant for organizationId specified in request data")
		assert.Equal(t, 2, countMockCalls(&temporalClient.Mock, "ExecuteWorkflow"))
	})

	t.Run("tenant allocated only on another site is rejected before proxy dispatch", func(t *testing.T) {
		recorder, _ := performRequest(model.APIReencryptTenantIdentitySecretsRequest{
			OrganizationID: cutil.GetPtr(otherSiteTenantOrg),
		})
		assert.Equal(t, http.StatusBadRequest, recorder.Code)
		assert.Contains(t, recorder.Body.String(), "Tenant organization does not have an allocation on the Site")
		assert.Equal(t, 2, countMockCalls(&temporalClient.Mock, "ExecuteWorkflow"))
	})

	temporalClient.AssertExpectations(t)
}

// TestGetJWKS_AbsentCasesReturn404AndPresentPassesThrough verifies absent JWKS paths return 404 (including the "tenant has no allocation" case so already-issued JWT-SVIDs remain verifiable) and a present JWKS passes through unchanged.
func TestGetJWKS_AbsentCasesReturn404AndPresentPassesThrough(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil)))

	const (
		tenantOrg        = "test-jwks-tenant"
		noAllocTenantOrg = "test-jwks-tenant-no-alloc"
		unknownOrg       = "no-such-tenant"
		providerOrg      = "test-jwks-provider"
	)
	tenantUser := testVPCBuildUser(t, dbSession, "test-jwks-tenant-user", tenantOrg, []string{auth.TenantAdminRole})
	tenant := testVPCBuildTenant(t, dbSession, "test-jwks-tenant", tenantOrg, tenantUser)
	noAllocTenantUser := testVPCBuildUser(t, dbSession, "test-jwks-tenant-user-noalloc", noAllocTenantOrg, []string{auth.TenantAdminRole})
	_ = testVPCBuildTenant(t, dbSession, "test-jwks-tenant-noalloc", noAllocTenantOrg, noAllocTenantUser)
	providerUser := testVPCBuildUser(t, dbSession, "test-jwks-provider-user", providerOrg, []string{auth.ProviderAdminRole})
	infraProvider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-jwks-infra-provider", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, infraProvider, "test-jwks-site", false, false, cdbm.SiteStatusRegistered, providerUser)
	_ = testBuildAllocation(t, dbSession, site, tenant, "test-jwks-alloc", tenantUser)

	testConfig := common.GetTestConfig()
	temporalCfg, _ := testConfig.GetTemporalConfig()
	temporalClient := &tmocks.Client{}
	siteClientPool := sc.NewClientPool(temporalCfg)
	siteClientPool.IDClientMap[site.ID.String()] = temporalClient

	notFoundRun := &tmocks.WorkflowRun{}
	notFoundRun.On("GetID").Return("test-jwks-notfound-wf-id")
	notFoundRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(grpcstatus.Error(grpccodes.NotFound, "tenant has no identity config"))

	successRun := &tmocks.WorkflowRun{}
	successRun.On("GetID").Return("test-jwks-success-wf-id")
	successRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(nil).Run(func(args mock.Arguments) {
		out := args.Get(1).(*corev1.Jwks)
		out.Jwks = `{"keys":[{"kty":"EC","kid":"real-key-id","alg":"ES256","crv":"P-256","x":"xxxx","y":"yyyy"}]}`
	})

	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything,
		mock.MatchedBy(func(opts tclient.StartWorkflowOptions) bool { return strings.HasSuffix(opts.ID, "-Spiffe") }),
		mock.AnythingOfType("string"),
		mock.Anything,
	).Return(notFoundRun, nil)
	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything,
		mock.MatchedBy(func(opts tclient.StartWorkflowOptions) bool { return strings.HasSuffix(opts.ID, "-Oidc") }),
		mock.AnythingOfType("string"),
		mock.Anything,
	).Return(successRun, nil)

	const realJWKSBody = `{"keys":[{"kty":"EC","kid":"real-key-id","alg":"ES256","crv":"P-256","x":"xxxx","y":"yyyy"}]}`

	echoSrv := echo.New()
	siteIDStr := site.ID.String()
	bogusSiteID := "00000000-0000-0000-0000-000000000000"

	tests := []struct {
		name       string
		orgName    string
		siteID     string
		kind       corev1.JwksKind
		wantStatus int
		wantBody   string
	}{
		{name: "absent: unknown site", orgName: tenantOrg, siteID: bogusSiteID, kind: corev1.JwksKind_Oidc, wantStatus: http.StatusNotFound},
		{name: "absent: org is not a Tenant", orgName: unknownOrg, siteID: siteIDStr, kind: corev1.JwksKind_Oidc, wantStatus: http.StatusNotFound},
		{name: "absent: Core gRPC API NOT_FOUND", orgName: tenantOrg, siteID: siteIDStr, kind: corev1.JwksKind_Spiffe, wantStatus: http.StatusNotFound},
		{name: "present: real JWKS pass-through", orgName: tenantOrg, siteID: siteIDStr, kind: corev1.JwksKind_Oidc, wantStatus: http.StatusOK, wantBody: realJWKSBody},
		// Tenant without an allocation on the site still resolves to JWKS so already-issued JWT-SVIDs remain verifiable.
		{name: "present: tenant has no allocation, controller has keys", orgName: noAllocTenantOrg, siteID: siteIDStr, kind: corev1.JwksKind_Oidc, wantStatus: http.StatusOK, wantBody: realJWKSBody},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			httpReq := httptest.NewRequest(http.MethodGet, "/", nil)
			recorder := httptest.NewRecorder()
			echoCtx := echoSrv.NewContext(httpReq, recorder)
			echoCtx.SetParamNames("orgName", "siteID")
			echoCtx.SetParamValues(tt.orgName, tt.siteID)

			require.NoError(t, NewGetJWKSHandler(dbSession, siteClientPool, tt.kind).Handle(echoCtx))
			assert.Equal(t, tt.wantStatus, recorder.Code, "body=%s", recorder.Body.String())
			if tt.wantBody != "" {
				assert.JSONEq(t, tt.wantBody, recorder.Body.String())
				assert.Contains(t, recorder.Header().Get(echo.HeaderContentType), echo.MIMEApplicationJSON)
			}
		})
	}
}

// TestGetJWKS_BodyValidation verifies how the JWKS handler classifies Core-returned bodies into 200 OK, 404 Not Found (empty body), and 502 Bad Gateway (malformed JSON).
func TestGetJWKS_BodyValidation(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil)))

	const (
		tenantOrg   = "test-jwks-bodyval-tenant"
		providerOrg = "test-jwks-bodyval-provider"
	)
	tenantUser := testVPCBuildUser(t, dbSession, "test-jwks-bodyval-tenant-user", tenantOrg, []string{auth.TenantAdminRole})
	tenant := testVPCBuildTenant(t, dbSession, "test-jwks-bodyval-tenant", tenantOrg, tenantUser)
	providerUser := testVPCBuildUser(t, dbSession, "test-jwks-bodyval-provider-user", providerOrg, []string{auth.ProviderAdminRole})
	infraProvider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-jwks-bodyval-infra-provider", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, infraProvider, "test-jwks-bodyval-site", false, false, cdbm.SiteStatusRegistered, providerUser)
	_ = testBuildAllocation(t, dbSession, site, tenant, "test-jwks-bodyval-alloc", tenantUser)

	testConfig := common.GetTestConfig()

	type bodyCase struct {
		name           string
		controllerBody string
		wantCode       int
		wantBody       string
	}
	cases := []bodyCase{
		{name: "empty body -> 404 Not Found", controllerBody: "", wantCode: http.StatusNotFound},
		{name: "whitespace body -> 404 Not Found", controllerBody: "  \n\t ", wantCode: http.StatusNotFound},
		{name: "non-JSON body -> 502 Bad Gateway", controllerBody: "not-json", wantCode: http.StatusBadGateway},
		{name: "JSON without keys field -> 502 Bad Gateway", controllerBody: `{"foo":"bar"}`, wantCode: http.StatusBadGateway},
		{name: "JSON array (not object) -> 502 Bad Gateway", controllerBody: `[]`, wantCode: http.StatusBadGateway},
		{name: "keys field null -> 502 Bad Gateway", controllerBody: `{"keys":null}`, wantCode: http.StatusBadGateway},
		{name: "keys field is a string -> 502 Bad Gateway", controllerBody: `{"keys":"not-an-array"}`, wantCode: http.StatusBadGateway},
		{name: "keys field is an object -> 502 Bad Gateway", controllerBody: `{"keys":{"foo":"bar"}}`, wantCode: http.StatusBadGateway},
		{name: "empty keys array passes through unchanged", controllerBody: `{"keys":[]}`,
			wantCode: http.StatusOK, wantBody: `{"keys":[]}`},
		{name: "valid keyset passes through unchanged", controllerBody: `{"keys":[{"kty":"EC","kid":"k1"}]}`,
			wantCode: http.StatusOK, wantBody: `{"keys":[{"kty":"EC","kid":"k1"}]}`},
	}

	echoSrv := echo.New()
	siteIDStr := site.ID.String()
	temporalCfg, _ := testConfig.GetTemporalConfig()

	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			temporalClient := &tmocks.Client{}
			siteClientPool := sc.NewClientPool(temporalCfg)
			siteClientPool.IDClientMap[site.ID.String()] = temporalClient

			run := &tmocks.WorkflowRun{}
			run.On("GetID").Return("test-jwks-bodyval-wf-id")
			controllerBody := tt.controllerBody
			run.Mock.On("Get", mock.Anything, mock.Anything).
				Return(nil).Run(func(args mock.Arguments) {
				out := args.Get(1).(*corev1.Jwks)
				out.Jwks = controllerBody
			})
			temporalClient.Mock.On("ExecuteWorkflow",
				mock.Anything, mock.Anything, mock.AnythingOfType("string"), mock.Anything,
			).Return(run, nil)

			httpReq := httptest.NewRequest(http.MethodGet, "/", nil)
			recorder := httptest.NewRecorder()
			echoCtx := echoSrv.NewContext(httpReq, recorder)
			echoCtx.SetParamNames("orgName", "siteID")
			echoCtx.SetParamValues(tenantOrg, siteIDStr)

			require.NoError(t, NewGetJWKSHandler(dbSession, siteClientPool, corev1.JwksKind_Oidc).Handle(echoCtx))
			assert.Equal(t, tt.wantCode, recorder.Code, "body=%s", recorder.Body.String())
			if tt.wantBody != "" {
				assert.JSONEq(t, tt.wantBody, recorder.Body.String())
				assert.Contains(t, recorder.Header().Get(echo.HeaderContentType), echo.MIMEApplicationJSON)
			}
		})
	}
}

// TestTenantIdentityPUT_WorkflowIDIncludesPayloadHash verifies tenant identity PUT workflow IDs include a payload hash so identical bodies produce the same ID and different bodies produce different IDs.
func TestTenantIdentityPUT_WorkflowIDIncludesPayloadHash(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil)))

	const (
		tenantOrg   = "test-payload-hash-tenant"
		providerOrg = "test-payload-hash-provider"
	)
	tenantUser := testVPCBuildUser(t, dbSession, "test-payload-hash-tenant-user", tenantOrg, []string{auth.TenantAdminRole})
	_ = testVPCBuildTenant(t, dbSession, "test-payload-hash-tenant", tenantOrg, tenantUser)
	providerUser := testVPCBuildUser(t, dbSession, "test-payload-hash-provider-user", providerOrg, []string{auth.ProviderAdminRole})
	infraProvider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-payload-hash-infra-provider", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, infraProvider, "test-payload-hash-site", false, false, cdbm.SiteStatusRegistered, providerUser)

	testConfig := common.GetTestConfig()
	temporalCfg, _ := testConfig.GetTemporalConfig()
	temporalClient := &tmocks.Client{}
	siteClientPool := sc.NewClientPool(temporalCfg)
	siteClientPool.IDClientMap[site.ID.String()] = temporalClient

	var capturedIDs []string
	run := &tmocks.WorkflowRun{}
	run.On("GetID").Return("test-payload-hash-wf-id")
	run.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything,
		mock.MatchedBy(func(opts tclient.StartWorkflowOptions) bool {
			capturedIDs = append(capturedIDs, opts.ID)
			return true
		}),
		mock.AnythingOfType("string"),
		mock.Anything,
	).Return(run, nil)

	echoSrv := echo.New()
	siteIDStr := site.ID.String()

	doPUT := func(t *testing.T, handler echo.HandlerFunc, body []byte) {
		t.Helper()
		httpReq := httptest.NewRequest(http.MethodPut, "/", strings.NewReader(string(body)))
		httpReq.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		recorder := httptest.NewRecorder()
		echoCtx := echoSrv.NewContext(httpReq, recorder)
		echoCtx.SetParamNames("orgName", "siteID")
		echoCtx.SetParamValues(tenantOrg, siteIDStr)
		echoCtx.Set("user", tenantUser)
		require.NoError(t, handler(echoCtx))
		require.Equalf(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
	}

	assertHashInvariant := func(t *testing.T, wantPrefix string, ids []string) {
		t.Helper()
		require.Len(t, ids, 3)
		assert.NotEqual(t, ids[0], ids[1], "different payloads must produce different IDs")
		assert.Equal(t, ids[0], ids[2], "identical payloads must produce identical IDs")
		assert.True(t, strings.HasPrefix(ids[0], wantPrefix), "ID must keep op-org-site prefix: %q", ids[0])
	}

	t.Run("PUT tenant-identity/config", func(t *testing.T) {
		makeConfigBody := func(audience string) []byte {
			body, err := json.Marshal(model.APITenantIdentityConfigCreateOrUpdateRequest{
				Enabled:         cutil.GetPtr(true),
				DefaultAudience: audience,
				Issuer:          "https://issuer.test/" + tenantOrg,
				TokenTtlSeconds: 3600,
			})
			require.NoError(t, err)
			return body
		}
		base := len(capturedIDs)
		handler := NewCreateOrUpdateTenantIdentityConfigHandler(dbSession, siteClientPool).Handle
		doPUT(t, handler, makeConfigBody("openbao-A"))
		doPUT(t, handler, makeConfigBody("openbao-B"))
		doPUT(t, handler, makeConfigBody("openbao-A"))
		assertHashInvariant(t, "tenant-identity-config-create-or-update-"+tenantOrg+"-"+siteIDStr+"-", capturedIDs[base:base+3])
	})

	t.Run("PUT tenant-identity/token-delegation", func(t *testing.T) {
		makeTokenDelegationBody := func(endpoint string) []byte {
			body, err := json.Marshal(model.APITenantIdentityTokenDelegationCreateOrUpdateRequest{
				TokenEndpoint:        endpoint,
				SubjectTokenAudience: "exchange-aud",
			})
			require.NoError(t, err)
			return body
		}
		base := len(capturedIDs)
		handler := NewCreateOrUpdateTenantIdentityTokenDelegationHandler(dbSession, siteClientPool).Handle
		doPUT(t, handler, makeTokenDelegationBody("https://auth-a.example.com/oauth2/token"))
		doPUT(t, handler, makeTokenDelegationBody("https://auth-b.example.com/oauth2/token"))
		doPUT(t, handler, makeTokenDelegationBody("https://auth-a.example.com/oauth2/token"))
		assertHashInvariant(t, "tenant-identity-token-delegation-create-or-update-"+tenantOrg+"-"+siteIDStr+"-", capturedIDs[base:base+3])
	})
}

// TestCreateOrUpdateTenantIdentityPUT_StatusReflectsCreateVsUpdate verifies tenant identity and token-delegation PUT handlers return 201 on first create and 200 on subsequent update.
func TestCreateOrUpdateTenantIdentityPUT_StatusReflectsCreateVsUpdate(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil)))
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil)))

	const (
		tenantOrg   = "test-status-tenant"
		providerOrg = "test-status-provider"
	)
	tenantUser := testVPCBuildUser(t, dbSession, "test-status-tenant-user", tenantOrg, []string{auth.TenantAdminRole})
	_ = testVPCBuildTenant(t, dbSession, "test-status-tenant", tenantOrg, tenantUser)
	providerUser := testVPCBuildUser(t, dbSession, "test-status-provider-user", providerOrg, []string{auth.ProviderAdminRole})
	infraProvider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-status-infra-provider", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, infraProvider, "test-status-site", false, false, cdbm.SiteStatusRegistered, providerUser)

	testConfig := common.GetTestConfig()
	temporalCfg, _ := testConfig.GetTemporalConfig()
	temporalClient := &tmocks.Client{}
	siteClientPool := sc.NewClientPool(temporalCfg)
	siteClientPool.IDClientMap[site.ID.String()] = temporalClient

	now := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)
	later := now.Add(time.Minute)

	createdConfigRun := &tmocks.WorkflowRun{}
	createdConfigRun.On("GetID").Return("test-status-config-create-wf-id")
	createdConfigRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(nil).Run(func(args mock.Arguments) {
		out := args.Get(1).(*corev1.TenantIdentityConfigResponse)
		out.OrganizationId = tenantOrg
		out.CreatedAt = timestamppb.New(now)
		out.UpdatedAt = timestamppb.New(now)
	})
	updatedConfigRun := &tmocks.WorkflowRun{}
	updatedConfigRun.On("GetID").Return("test-status-config-update-wf-id")
	updatedConfigRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(nil).Run(func(args mock.Arguments) {
		out := args.Get(1).(*corev1.TenantIdentityConfigResponse)
		out.OrganizationId = tenantOrg
		out.CreatedAt = timestamppb.New(now)
		out.UpdatedAt = timestamppb.New(later)
	})

	createdDelegationRun := &tmocks.WorkflowRun{}
	createdDelegationRun.On("GetID").Return("test-status-delegation-create-wf-id")
	createdDelegationRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(nil).Run(func(args mock.Arguments) {
		out := args.Get(1).(*corev1.TokenDelegationResponse)
		out.OrganizationId = tenantOrg
		out.CreatedAt = timestamppb.New(now)
		out.UpdatedAt = timestamppb.New(now)
	})
	updatedDelegationRun := &tmocks.WorkflowRun{}
	updatedDelegationRun.On("GetID").Return("test-status-delegation-update-wf-id")
	updatedDelegationRun.Mock.On("Get", mock.Anything, mock.Anything).
		Return(nil).Run(func(args mock.Arguments) {
		out := args.Get(1).(*corev1.TokenDelegationResponse)
		out.OrganizationId = tenantOrg
		out.CreatedAt = timestamppb.New(now)
		out.UpdatedAt = timestamppb.New(later)
	})

	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything, mock.Anything,
		"CreateOrUpdateTenantIdentityConfiguration", mock.Anything,
	).Return(createdConfigRun, nil).Once()
	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything, mock.Anything,
		"CreateOrUpdateTenantIdentityConfiguration", mock.Anything,
	).Return(updatedConfigRun, nil).Once()
	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything, mock.Anything,
		"CreateOrUpdateTenantIdentityTokenDelegation", mock.Anything,
	).Return(createdDelegationRun, nil).Once()
	temporalClient.Mock.On("ExecuteWorkflow",
		mock.Anything, mock.Anything,
		"CreateOrUpdateTenantIdentityTokenDelegation", mock.Anything,
	).Return(updatedDelegationRun, nil).Once()

	echoSrv := echo.New()
	siteIDStr := site.ID.String()

	doConfigPUT := func(t *testing.T, audience string) *httptest.ResponseRecorder {
		t.Helper()
		body, err := json.Marshal(model.APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled:         cutil.GetPtr(true),
			DefaultAudience: audience,
			Issuer:          "https://issuer.test/" + tenantOrg,
			TokenTtlSeconds: 3600,
		})
		require.NoError(t, err)
		httpReq := httptest.NewRequest(http.MethodPut, "/", strings.NewReader(string(body)))
		httpReq.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		recorder := httptest.NewRecorder()
		echoCtx := echoSrv.NewContext(httpReq, recorder)
		echoCtx.SetParamNames("orgName", "siteID")
		echoCtx.SetParamValues(tenantOrg, siteIDStr)
		echoCtx.Set("user", tenantUser)
		require.NoError(t, NewCreateOrUpdateTenantIdentityConfigHandler(dbSession, siteClientPool).Handle(echoCtx))
		return recorder
	}
	doDelegationPUT := func(t *testing.T, endpoint string) *httptest.ResponseRecorder {
		t.Helper()
		body, err := json.Marshal(model.APITenantIdentityTokenDelegationCreateOrUpdateRequest{
			TokenEndpoint:        endpoint,
			SubjectTokenAudience: "exchange-aud",
		})
		require.NoError(t, err)
		httpReq := httptest.NewRequest(http.MethodPut, "/", strings.NewReader(string(body)))
		httpReq.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		recorder := httptest.NewRecorder()
		echoCtx := echoSrv.NewContext(httpReq, recorder)
		echoCtx.SetParamNames("orgName", "siteID")
		echoCtx.SetParamValues(tenantOrg, siteIDStr)
		echoCtx.Set("user", tenantUser)
		require.NoError(t, NewCreateOrUpdateTenantIdentityTokenDelegationHandler(dbSession, siteClientPool).Handle(echoCtx))
		return recorder
	}

	t.Run("tenant-identity/config first create returns 201", func(t *testing.T) {
		recorder := doConfigPUT(t, "openbao-create")
		assert.Equal(t, http.StatusCreated, recorder.Code, "body=%s", recorder.Body.String())
	})
	t.Run("tenant-identity/config subsequent update returns 200", func(t *testing.T) {
		recorder := doConfigPUT(t, "openbao-update")
		assert.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
	})
	t.Run("token-delegation first create returns 201", func(t *testing.T) {
		recorder := doDelegationPUT(t, "https://auth-create.example.com/oauth2/token")
		assert.Equal(t, http.StatusCreated, recorder.Code, "body=%s", recorder.Body.String())
	})
	t.Run("token-delegation subsequent update returns 200", func(t *testing.T) {
		recorder := doDelegationPUT(t, "https://auth-update.example.com/oauth2/token")
		assert.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
	})
}
