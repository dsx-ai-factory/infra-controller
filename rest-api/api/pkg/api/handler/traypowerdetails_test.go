// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// TestGetTrayPowerDetailsHandler_Handle proves authorization precedes dispatch,
// and that GET preserves proxy errors and independently available data.
func TestGetTrayPowerDetailsHandler_Handle(t *testing.T) {
	dbSession := testTrayInitDB(t)
	defer dbSession.Close()
	org := "power-details-org"
	_, site, _ := testTraySetupTestData(t, dbSession, org)

	// The ownership check only needs another provider and site. Reusing the
	// full tray fixture would also create a second, irrelevant tenant account.
	otherProvider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "other-power-provider", Org: "other-power-org"}
	_, err := dbSession.DB.NewInsert().Model(otherProvider).Exec(context.Background())
	require.NoError(t, err)
	otherSite := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "other-power-site",
		Org:                      otherProvider.Org,
		InfrastructureProviderID: otherProvider.ID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{Flow: true},
	}
	_, err = dbSession.DB.NewInsert().Model(otherSite).Exec(context.Background())
	require.NoError(t, err)
	provider := testTrayBuildUser(t, dbSession, "power-details-provider", org, []string{authz.ProviderAdminRole})
	tenant := testTrayBuildUser(t, dbSession, "power-details-tenant", org, []string{authz.TenantAdminRole})
	noProvider := testTrayBuildUser(t, dbSession, "power-details-no-provider", "no-provider", []string{authz.ProviderAdminRole})

	// Same ownership, but no Flow service: this must fail before obtaining a client.
	noFlow := &cdbm.Site{ID: uuid.New(), Name: "no-flow", Org: org, InfrastructureProviderID: site.InfrastructureProviderID, Status: cdbm.SiteStatusRegistered}
	_, err = dbSession.DB.NewInsert().Model(noFlow).Exec(context.Background())
	require.NoError(t, err)

	partial := &flowv1.GetTrayPowerDetailsResponse{Details: &flowv1.TrayPowerDetails{
		FlowComponentId: &flowv1.UUID{Id: uuid.NewString()}, ComponentType: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
		Actual:     &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED}},
		Operations: &flowv1.PowerOperations{Tasks: &flowv1.PowerTaskDetailsList{}},
	}}
	cases := []struct {
		name       string
		user       *cdbm.User
		org        string
		siteID     string
		selector   string
		dispatch   bool
		response   *flowv1.GetTrayPowerDetailsResponse
		proxyError error
		status     int
	}{
		// An unavailable hardware section still yields usable empty task information.
		{name: "partial details", user: provider, org: org, siteID: site.ID.String(), selector: "managed-compute", dispatch: true, response: partial, status: http.StatusOK},
		// Forward a MAC selector unchanged so Flow can resolve inventory identity.
		{name: "MAC selector", user: provider, org: org, siteID: site.ID.String(), selector: "d8:ab:cd:ef:00:01", dispatch: true, response: partial, status: http.StatusOK},
		// A missing authenticated user must never reach the site proxy.
		{name: "missing user", org: org, siteID: site.ID.String(), status: http.StatusInternalServerError},
		// A provider role in one org does not authorize reading another org.
		{name: "different org", user: provider, org: "unrelated-org", siteID: site.ID.String(), status: http.StatusForbidden},
		// Site familiarity does not grant provider-only hardware access.
		{name: "tenant role", user: tenant, org: org, siteID: site.ID.String(), status: http.StatusForbidden},
		// A provider role must resolve to an actual provider record.
		{name: "missing provider", user: noProvider, org: "no-provider", siteID: site.ID.String(), status: http.StatusBadRequest},
		// An omitted selector must not become a default site.
		{name: "missing site", user: provider, org: org, status: http.StatusBadRequest},
		// Reject UUID syntax before the database or proxy interprets it.
		{name: "malformed site", user: provider, org: org, siteID: "not-a-uuid", status: http.StatusBadRequest},
		// A valid UUID is not evidence that the site exists.
		{name: "unknown site", user: provider, org: org, siteID: uuid.NewString(), status: http.StatusBadRequest},
		// A valid foreign site still belongs to another provider.
		{name: "other provider site", user: provider, org: org, siteID: otherSite.ID.String(), status: http.StatusForbidden},
		// Site ownership does not enable a service that is disabled.
		{name: "Flow disabled", user: provider, org: org, siteID: noFlow.ID.String(), status: http.StatusPreconditionFailed},
		// Older services remain distinguishable from a successful response with no work.
		{name: "older Flow", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: swe.WrapErr(status.Error(codes.Unimplemented, "method unavailable")), status: http.StatusNotImplemented},
		// Missing inventory and unavailable services retain the proxy's HTTP classification.
		{name: "tray absent", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: swe.WrapErr(status.Error(codes.NotFound, "tray absent")), status: http.StatusNotFound},
		// Preserve a dependency outage instead of presenting empty details.
		{name: "Flow unavailable", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: swe.WrapErr(status.Error(codes.Unavailable, "service unavailable")), status: http.StatusServiceUnavailable},
		// A workflow timeout must not be followed by a successful empty payload.
		{name: "proxy deadline", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), status: http.StatusGatewayTimeout},
		// An empty successful envelope contains no evidence that the tray was found.
		{name: "missing details", user: provider, org: org, siteID: site.ID.String(), dispatch: true, response: &flowv1.GetTrayPowerDetailsResponse{}, status: http.StatusBadGateway},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// The cached mock handles every dispatched request; no live Temporal
			// connection or local client certificate belongs in this test.
			pool := sc.NewClientPool(nil)
			client := &tmocks.Client{}
			pool.IDClientMap[site.ID.String()] = client
			if tc.dispatch {
				run := &tmocks.WorkflowRun{}
				if tc.proxyError != nil {
					testFlowProxyFailure(run, tc.proxyError)
				} else {
					testFlowProxyReply(t, run, tc.response)
				}
				started := testFlowProxyMethodDispatch(t, client, run, flowv1.Flow_GetTrayPowerDetails_FullMethodName, func(args mock.Arguments) {
					request := &flowv1.GetTrayPowerDetailsRequest{}
					testFlowProxyRequest(t, args, request)
					assert.Equal(t, tc.selector, request.GetId())
				})
				t.Cleanup(func() {
					request := &flowv1.GetTrayPowerDetailsRequest{Id: tc.selector}
					assert.Equal(t, "tray-power-details-"+common.RequestHash(request), started.ID)
					assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, started.WorkflowIDConflictPolicy)
				})
			}

			request := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/v2/org/%s/nico/tray/%s/power?siteId=%s", tc.org, tc.selector, tc.siteID), nil)
			recorder := httptest.NewRecorder()
			ec := echo.New().NewContext(request, recorder)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.org, tc.selector)
			ec.Set("user", tc.user)

			err := NewGetTrayPowerDetailsHandler(dbSession, pool).Handle(ec)
			require.NoError(t, err)
			require.Equal(t, tc.status, recorder.Code, recorder.Body.String())
			calls := 0
			if tc.dispatch {
				calls = 1
			}
			client.AssertNumberOfCalls(t, "ExecuteWorkflow", calls)
			if tc.status != http.StatusOK {
				var response map[string]any
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				assert.Contains(t, response, "message")
				assert.NotContains(t, response, "actual")
				return
			}

			var response model.APITrayPowerDetails
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
			require.NotNil(t, response.Actual.Error)
			require.NotNil(t, response.Operations.Actions.Error)
			assert.Equal(t, model.APIPowerDetailsError("ReadFailed"), *response.Actual.Error)
			assert.Equal(t, model.APIPowerDetailsError("Unsupported"), *response.Operations.Actions.Error)
			assert.NotNil(t, response.Operations.Tasks.Entries)
			assert.Empty(t, response.Operations.Tasks.Entries)
			assert.Nil(t, response.Operations.Tasks.Error)
		})
	}
}
