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

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// TestGetRackPowerStateHandler_Handle proves this route rejects unauthorized reads before dispatch,
// sends the external rack selector once, and retains partial results and size failures for the whole response.
func TestGetRackPowerStateHandler_Handle(t *testing.T) {
	dbSession := testTrayInitDB(t)
	defer dbSession.Close()
	org := "rack-power-state-org"
	_, site, _ := testTraySetupTestData(t, dbSession, org)
	provider := testTrayBuildUser(t, dbSession, "rack-power-admin", org, []string{authz.ProviderAdminRole})
	viewer := testTrayBuildUser(t, dbSession, "rack-power-viewer", org, []string{authz.ProviderViewerRole})
	noProvider := testTrayBuildUser(t, dbSession, "rack-power-no-provider", "rack-no-provider", []string{authz.ProviderAdminRole})
	// Only provider/site ownership matters here; no foreign tray inventory is needed.
	otherSite := &cdbm.Site{ID: uuid.New(), Name: "foreign-rack-power-site", Org: "foreign-org",
		InfrastructureProviderID: uuid.New(), Status: cdbm.SiteStatusRegistered, Config: &cdbm.SiteConfig{Flow: true}}
	otherProvider := &cdbm.InfrastructureProvider{ID: otherSite.InfrastructureProviderID, Name: "foreign-rack-power-provider", Org: "foreign-org"}
	_, err := dbSession.DB.NewInsert().Model(otherProvider).Exec(context.Background())
	require.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(otherSite).Exec(context.Background())
	require.NoError(t, err)
	// The provider owns this site, but ownership does not enable Flow.
	noFlow := &cdbm.Site{ID: uuid.New(), Name: "rack-power-no-flow", Org: org,
		InfrastructureProviderID: site.InfrastructureProviderID, Status: cdbm.SiteStatusRegistered}
	_, err = dbSession.DB.NewInsert().Model(noFlow).Exec(context.Background())
	require.NoError(t, err)

	rackID := "inventory-rack"
	unlinked := &flowv1.RackTrayPowerDetails{
		ComponentType: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
		Actual:        &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED}},
		Desired:       &flowv1.PowerDesiredDetails{Result: &flowv1.PowerDesiredDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED}},
		Operations: &flowv1.RackTrayPowerOperations{
			Actions: &flowv1.PowerActionDetails{Result: &flowv1.PowerActionDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED}},
			Tasks:   &flowv1.PowerTaskReferenceList{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED.Enum()},
		},
	}
	partial := &flowv1.GetRackPowerDetailsResponse{Details: &flowv1.RackPowerDetails{
		RackExternalId: rackID,
		Actual: &flowv1.RackPowerSummary{State: flowv1.RackPowerState_RACK_POWER_STATE_UNKNOWN,
			Coverage: &flowv1.RackPowerCoverage{Total: 1, Unavailable: 1}},
		Desired: &flowv1.RackPowerSummary{State: flowv1.RackPowerState_RACK_POWER_STATE_UNKNOWN,
			Coverage: &flowv1.RackPowerCoverage{Total: 1, Unavailable: 1}},
		Operations: &flowv1.RackPowerOperations{
			Actions: []*flowv1.RackMemberPowerActions{{ComponentType: unlinked.ComponentType, Details: unlinked.Operations.Actions}},
			Tasks:   &flowv1.PowerTaskDetailsList{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED.Enum()},
		},
		Trays: []*flowv1.RackTrayPowerDetails{unlinked},
	}}
	cases := []struct {
		name         string
		user         *cdbm.User
		org          string
		siteID       string
		selector     string
		wantSelector string
		boundRackID  string
		emptyRackID  bool
		wantRawPath  bool
		dispatch     bool
		response     *flowv1.GetRackPowerDetailsResponse
		proxyError   error
		status       int
		message      string
	}{
		// Failed live/task reads must not discard the rack or its inventory members.
		{name: "partial rack", user: provider, org: org, siteID: site.ID.String(), dispatch: true, response: partial, status: http.StatusOK},
		// A second decode would change the inventory ID's literal percent escape.
		{name: "external selector decoded once", user: provider, org: org, siteID: site.ID.String(), selector: "rack%252Flabel",
			wantSelector: "rack%2Flabel", dispatch: true, response: partial, status: http.StatusOK},
		// Noncanonical escaped punctuation makes Echo use RawPath, which the handler must decode.
		{name: "escaped selector from RawPath", user: provider, org: org, siteID: site.ID.String(), selector: "rack%3Alabel",
			wantSelector: "rack:label", wantRawPath: true, dispatch: true, response: partial, status: http.StatusOK},
		// The literal route supplies this external ID without an :id parameter.
		{name: "bound task rack selector", user: provider, org: org, siteID: site.ID.String(), selector: "task",
			boundRackID: "task", dispatch: true, response: partial, status: http.StatusOK},
		// An empty decoded selector must fail before obtaining a site client or dispatching.
		{name: "empty rack selector", user: provider, org: org, siteID: site.ID.String(), emptyRackID: true,
			status: http.StatusBadRequest, message: "Rack identifier is required"},
		// A missing authenticated identity cannot reach the site proxy.
		{name: "missing user", org: org, siteID: site.ID.String(), status: http.StatusInternalServerError},
		// A role in one organization does not authorize another organization's rack.
		{name: "different org", user: provider, org: "unrelated-org", siteID: site.ID.String(), status: http.StatusForbidden},
		// Live hardware reads require Provider Admin rather than Provider Viewer.
		{name: "provider viewer", user: viewer, org: org, siteID: site.ID.String(), status: http.StatusForbidden},
		// Provider authorization alone does not create the required provider entity.
		{name: "missing provider", user: noProvider, org: "rack-no-provider", siteID: site.ID.String(),
			status: http.StatusBadRequest, message: "Current org does not have Infrastructure Provider initialized"},
		// An omitted site selector must not choose an implicit site.
		{name: "missing site", user: provider, org: org, status: http.StatusBadRequest, message: "siteId query parameter is required"},
		// Reject invalid UUID syntax before site lookup or dispatch.
		{name: "malformed site", user: provider, org: org, siteID: "not-a-uuid", status: http.StatusBadRequest, message: "Invalid siteId"},
		// Valid UUID syntax does not establish that a site exists.
		{name: "unknown site", user: provider, org: org, siteID: uuid.NewString(), status: http.StatusBadRequest, message: "Site specified in request does not exist"},
		// An existing site can still belong to a different infrastructure provider.
		{name: "foreign provider site", user: provider, org: org, siteID: otherSite.ID.String(), status: http.StatusForbidden},
		// Service configuration is checked before obtaining a Temporal client.
		{name: "Flow disabled", user: provider, org: org, siteID: noFlow.ID.String(), status: http.StatusPreconditionFailed},
		// An old Flow service is unavailable, rather than a successful empty rack.
		{name: "older Flow", user: provider, org: org, siteID: site.ID.String(), dispatch: true,
			proxyError: tp.NewNonRetryableApplicationError("method unavailable", swe.ErrTypeNICoUnimplemented, nil), status: http.StatusNotImplemented},
		// A size failure for the whole response remains an HTTP error, unlike a partial task error.
		{name: "whole rack exceeds size allowance", user: provider, org: org, siteID: site.ID.String(), dispatch: true,
			proxyError: tp.NewNonRetryableApplicationError("rack power details exceed the response size limit", swe.ErrTypeNICoResourceExhausted, nil), status: http.StatusTooManyRequests},
		// An injected workflow timeout must return 504 without waiting for a real deadline.
		{name: "proxy deadline", user: provider, org: org, siteID: site.ID.String(), dispatch: true,
			proxyError: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil),
			status:     http.StatusGatewayTimeout, message: "Flow proxy request timed out"},
		// A missing required payload must not become a rack containing default values.
		{name: "missing rack details", user: provider, org: org, siteID: site.ID.String(), dispatch: true,
			response: &flowv1.GetRackPowerDetailsResponse{}, status: http.StatusBadGateway},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			selector := tc.selector
			if selector == "" {
				selector = rackID
			}
			wantSelector := tc.wantSelector
			if wantSelector == "" {
				wantSelector = selector
			}
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
				started := testFlowProxyMethodDispatch(t, client, run, flowv1.Flow_GetRackPowerDetails_FullMethodName, func(args mock.Arguments) {
					request := &flowv1.GetRackPowerDetailsRequest{}
					testFlowProxyRequest(t, args, request)
					assert.Equal(t, wantSelector, request.GetId())
				})
				t.Cleanup(func() {
					request := &flowv1.GetRackPowerDetailsRequest{Id: wantSelector}
					assert.Equal(t, "rack-power-state-"+common.RequestHash(request), started.ID)
					assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, started.WorkflowIDConflictPolicy)
				})
			}

			// Echo chooses between Path and RawPath before the handler sees the selector.
			e := echo.New()
			e.Use(func(next echo.HandlerFunc) echo.HandlerFunc {
				return func(c echo.Context) error {
					c.Set("user", tc.user)
					return next(c)
				}
			})
			handler := NewGetRackPowerStateHandler(dbSession, pool)
			if tc.boundRackID != "" {
				handler = handler.ForRack(tc.boundRackID)
				e.GET("/v2/org/:orgName/nico/rack/task/power", handler.Handle)
			} else {
				e.GET("/v2/org/:orgName/nico/rack/:id/power", handler.Handle)
			}
			request := httptest.NewRequest(http.MethodGet,
				fmt.Sprintf("/v2/org/%s/nico/rack/%s/power?siteId=%s", tc.org, selector, tc.siteID), nil)
			recorder := httptest.NewRecorder()
			assert.Equal(t, tc.wantRawPath, request.URL.RawPath != "", "the selector fixture must exercise its intended URL branch")

			if tc.emptyRackID {
				// Echo normally requires a path segment; call the unbound handler
				// with an empty decoded parameter to exercise its selector guard.
				ctx := e.NewContext(request, recorder)
				ctx.SetParamNames("orgName", "id")
				ctx.SetParamValues(tc.org, "")
				ctx.Set("user", tc.user)
				require.NoError(t, handler.Handle(ctx))
			} else {
				e.ServeHTTP(recorder, request)
			}

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
				message := tc.message
				if message == "" && tc.proxyError != nil {
					message = tc.proxyError.Error()
				}
				if message != "" {
					assert.Equal(t, message, response["message"])
				}
				assert.Contains(t, response, "data")
				assert.Nil(t, response["data"])
				assert.NotContains(t, response, "actual")
				return
			}
			var response model.APIRackPowerState
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
			assert.Equal(t, &rackID, response.ID)
			assert.Equal(t, model.APIRackPowerSummaryState("Unknown"), response.Actual.State)
			assert.False(t, response.Actual.Complete)
			assert.Equal(t, uint32(1), response.Actual.Coverage.Unavailable)
			assert.Equal(t, model.APIRackPowerCoverage{Total: 1, Unavailable: 1}, response.Desired.Coverage)
			require.Len(t, response.Components, 1)
			assert.Equal(t, "Compute", response.Components[0].Type)
			assert.Equal(t, []model.APIPowerAction{}, response.Components[0].Operations.Actions)
			assert.Equal(t, cutil.GetPtr[model.APIPowerReadError]("NotLinked"), response.Components[0].Operations.ActionsError)
			assert.Nil(t, response.Components[0].ID)
			assert.Equal(t, cutil.GetPtr[model.APIPowerReadError]("NotLinked"), response.Components[0].Actual.Error)
			assert.Equal(t, cutil.GetPtr[model.APIPowerReadError]("ReadFailed"), response.Operations.TasksError)
			assert.Equal(t, cutil.GetPtr[model.APIPowerReadError]("ReadFailed"), response.Components[0].Operations.TasksError)
		})
	}
}
