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
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// TestGetTrayPowerStateHandler_Handle proves authorization precedes dispatch,
// and that GET preserves proxy errors and independently available data.
func TestGetTrayPowerStateHandler_Handle(t *testing.T) {
	dbSession := testTrayInitDB(t)
	defer dbSession.Close()
	org := "power-state-org"
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
	provider := testTrayBuildUser(t, dbSession, "power-state-provider", org, []string{authz.ProviderAdminRole})
	tenant := testTrayBuildUser(t, dbSession, "power-state-tenant", org, []string{authz.TenantAdminRole})
	viewer := testTrayBuildUser(t, dbSession, "power-state-viewer", org, []string{authz.ProviderViewerRole})
	noProvider := testTrayBuildUser(t, dbSession, "power-state-no-provider", "no-provider", []string{authz.ProviderAdminRole})

	// Same ownership, but no Flow service: this must fail before obtaining a client.
	noFlow := &cdbm.Site{ID: uuid.New(), Name: "no-flow", Org: org, InfrastructureProviderID: site.InfrastructureProviderID, Status: cdbm.SiteStatusRegistered}
	_, err = dbSession.DB.NewInsert().Model(noFlow).Exec(context.Background())
	require.NoError(t, err)

	// Close a separate pool so a provider query failure cannot disable
	// the shared fixtures used by the remaining cases.
	closedSession := cdbu.GetTestDBSession(t, false)
	closedSession.Close()

	componentID, rackID := "managed-compute", "inventory-rack"
	taskID := uuid.NewString()
	partial := &flowv1.GetTrayPowerDetailsResponse{Details: &flowv1.TrayPowerDetails{
		ComponentId: &componentID, RackExternalId: &rackID, ComponentType: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
		Actual: &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED}},
		Operations: &flowv1.PowerOperations{Tasks: &flowv1.PowerTaskDetailsList{Entries: []*flowv1.PowerTaskDetails{{
			Id:     &flowv1.UUID{Id: taskID},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			ExternalTargets: &flowv1.PowerTaskExternalTargets{
				Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum(),
			},
		}}}},
	}}
	cases := []struct {
		name             string
		user             *cdbm.User
		org              string
		siteID           string
		selector         string
		expectedSelector string
		dbSession        *cdb.Session
		dispatch         bool
		response         *flowv1.GetTrayPowerDetailsResponse
		proxyError       error
		status           int
		message          string
	}{
		// Hardware and target-ID failures still leave task metadata available
		// through the proxy JSON response.
		{name: "partial details", user: provider, org: org, siteID: site.ID.String(), selector: "managed-compute", dispatch: true, response: partial, status: http.StatusOK},
		// Echo preserves escaped colons in RawPath; Flow needs the decoded MAC.
		{name: "encoded MAC selector", user: provider, org: org, siteID: site.ID.String(), selector: "d8%3Aab%3Acd%3Aef%3A00%3A01", expectedSelector: "d8:ab:cd:ef:00:01", dispatch: true, response: partial, status: http.StatusOK},
		// net/http already decodes a literal percent sign when RawPath is empty.
		{name: "literal percent selector", user: provider, org: org, siteID: site.ID.String(), selector: "unit%25label", expectedSelector: "unit%label", dispatch: true, response: partial, status: http.StatusOK},
		// A second decode would turn this identifier's literal %2F into a slash.
		{name: "selector decoded once", user: provider, org: org, siteID: site.ID.String(), selector: "unit%252Flabel", expectedSelector: "unit%2Flabel", dispatch: true, response: partial, status: http.StatusOK},
		// A missing authenticated user must never reach the site proxy.
		{name: "missing user", org: org, siteID: site.ID.String(), status: http.StatusInternalServerError},
		// A provider role in one org does not authorize reading another org.
		{name: "different org", user: provider, org: "unrelated-org", siteID: site.ID.String(), status: http.StatusForbidden, message: "Failed to validate membership for org"},
		// A Tenant Admin in the same org lacks the required Provider Admin role.
		{name: "tenant role", user: tenant, org: org, siteID: site.ID.String(), status: http.StatusForbidden},
		// Reading live power state requires Provider Admin, not just a provider role.
		{name: "provider viewer", user: viewer, org: org, siteID: site.ID.String(), status: http.StatusForbidden},
		// A provider role must resolve to an actual provider record.
		{name: "missing provider", user: noProvider, org: "no-provider", siteID: site.ID.String(), status: http.StatusBadRequest, message: "Current org does not have Infrastructure Provider initialized"},
		// A failed provider query is a server error, not an uninitialized org.
		{name: "provider query failure", user: provider, org: org, siteID: site.ID.String(), dbSession: closedSession, status: http.StatusInternalServerError, message: "Failed to retrieve infrastructure provider for org, DB error"},
		// An omitted siteId must not become a default site.
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
		{name: "older Flow", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: tp.NewNonRetryableApplicationError("method unavailable", swe.ErrTypeNICoUnimplemented, nil), status: http.StatusNotImplemented},
		// Missing inventory and unavailable services retain the proxy's HTTP classification.
		{name: "tray absent", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: tp.NewNonRetryableApplicationError("tray absent", swe.ErrTypeNICoObjectNotFound, nil), status: http.StatusNotFound},
		// Preserve a dependency outage instead of presenting empty details.
		{name: "Flow unavailable", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: tp.NewNonRetryableApplicationError("service unavailable", swe.ErrTypeNICoUnavailable, nil), status: http.StatusServiceUnavailable},
		// A workflow timeout must not be followed by a successful empty payload.
		{name: "proxy deadline", user: provider, org: org, siteID: site.ID.String(), dispatch: true, proxyError: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), status: http.StatusGatewayTimeout},
		// An empty successful envelope contains no evidence that the tray was found.
		{name: "missing details", user: provider, org: org, siteID: site.ID.String(), dispatch: true, response: &flowv1.GetTrayPowerDetailsResponse{}, status: http.StatusBadGateway, message: "Flow returned no tray power state"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			selector := tc.selector
			if selector == "" {
				selector = componentID
			}
			expectedSelector := tc.expectedSelector
			if expectedSelector == "" {
				expectedSelector = selector
			}

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
					assert.Equal(t, expectedSelector, request.GetId())
				})
				t.Cleanup(func() {
					request := &flowv1.GetTrayPowerDetailsRequest{Id: expectedSelector}
					assert.Equal(t, "tray-power-state-"+common.RequestHash(request), started.ID)
					assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, started.WorkflowIDConflictPolicy)
				})
			}

			request := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/v2/org/%s/nico/tray/%s/power?siteId=%s", tc.org, selector, tc.siteID), nil)
			recorder := httptest.NewRecorder()
			session := dbSession
			if tc.dbSession != nil {
				session = tc.dbSession
			}
			handler := NewGetTrayPowerStateHandler(session, pool)
			e := echo.New()
			e.Use(func(next echo.HandlerFunc) echo.HandlerFunc {
				return func(c echo.Context) error {
					c.Set("user", tc.user)
					return next(c)
				}
			})
			e.GET("/v2/org/:orgName/nico/tray/:id/power", handler.Handle)
			e.ServeHTTP(recorder, request)
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
				if tc.message != "" {
					assert.Equal(t, tc.message, response["message"])
				}
				assert.Contains(t, response, "data")
				assert.Nil(t, response["data"])
				assert.NotContains(t, recorder.Body.String(), "sql: database is closed")
				assert.NotContains(t, response, "actual")
				return
			}

			var response model.APITrayPowerState
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
			assert.Equal(t, &componentID, response.ID)
			assert.Equal(t, &rackID, response.RackID)
			require.NotNil(t, response.Actual.Error)
			require.NotNil(t, response.Operations.ActionsError)
			assert.Equal(t, model.APIPowerReadError("ReadFailed"), *response.Actual.Error)
			assert.Equal(t, model.APIPowerReadError("Unsupported"), *response.Operations.ActionsError)
			assert.Nil(t, response.Operations.TasksError)
			require.Len(t, response.Operations.Tasks, 1)
			task := response.Operations.Tasks[0]
			assert.Equal(t, taskID, task.ID)
			assert.Equal(t, "Running", task.Status)
			assert.Nil(t, task.RackID)
			assert.Nil(t, task.Targets)
			require.NotNil(t, task.TargetsError)
			assert.Equal(t, model.APIPowerReadError("NotLinked"), *task.TargetsError)
		})
	}
}
