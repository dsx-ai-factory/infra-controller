// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestPutDefaultLeakagePolicyOverrideHandler_Handle(t *testing.T) {
	tests := []struct {
		name         string
		body         string
		wantStrategy flowv1.EventRuleTargetStrategy
		wantTarget   *string
		wantError    string
		querySite    bool
	}{
		{name: "missing field preserves decoder detail", body: `{"siteId":"{{siteId}}","action":"NoAction","targetStrategy":null}`, wantError: "request body must contain exactly siteId, action, targetStrategy, and onConflict"},
		{name: "siteId must be in body", body: `{"action":"NoAction","targetStrategy":null,"onConflict":null}`, wantError: "request body must contain exactly siteId, action, targetStrategy, and onConflict"},
		{name: "invalid siteId rejected before routing", body: `{"siteId":"not-a-uuid","action":"NoAction","targetStrategy":null,"onConflict":null}`, wantError: "siteId: siteId must be a valid UUID."},
		{name: "body Site must exist", body: `{"siteId":"11111111-1111-4111-8111-111111111111","action":"NoAction","targetStrategy":null,"onConflict":null}`, wantError: "Site specified in request does not exist"},
		{name: "query siteId rejected", body: `{"siteId":"{{siteId}}","action":"NoAction","targetStrategy":null,"onConflict":null}`, querySite: true, wantError: "Unknown query parameter specified in request: siteId"},
		{name: "no action", body: `{"siteId":"{{siteId}}","action":"NoAction","targetStrategy":null,"onConflict":null}`},
		{name: "source policy", body: `{"siteId":"{{siteId}}","action":"ForcePowerOff","targetStrategy":"LeakingSource","onConflict":"Queue"}`, wantStrategy: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE, wantTarget: stringPointer("LeakingSource")},
		{name: "rack policy", body: `{"siteId":"{{siteId}}","action":"ForcePowerOff","targetStrategy":"LeakingRack","onConflict":"Queue"}`, wantStrategy: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK, wantTarget: stringPointer("LeakingRack")},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			e := echo.New()
			dbSession := testRackInitDB(t)
			defer dbSession.Close()
			cfg := common.GetTestConfig()
			temporalConfig, _ := cfg.GetTemporalConfig()
			scp := sc.NewClientPool(temporalConfig)
			org := "test-org"
			_, site, _ := testRackSetupTestData(t, dbSession, org)
			providerUser := testRackBuildUser(t, dbSession, "provider-user-leakage-policy-put", org, []string{authz.ProviderAdminRole})
			mockTC := &tmocks.Client{}
			if test.wantError == "" {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("test-workflow-id")
				action := &flowv1.EventRuleAction{Name: "snapshot", Spec: &flowv1.EventRuleAction_Noop{Noop: &flowv1.EventRuleNoopAction{Reason: "record"}}}
				if test.wantTarget != nil {
					action.Spec = &flowv1.EventRuleAction_SubmitTask{SubmitTask: &flowv1.EventRuleSubmitTaskAction{
						TargetStrategy: test.wantStrategy, ConflictStrategy: flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE,
						Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF}}},
					}}
				}
				testFlowProxyReply(t, run, &flowv1.EventRule{EventType: leakageEventType, Enabled: true, Actions: []*flowv1.EventRuleAction{action}})
				testFlowProxyMethodDispatch(t, mockTC, run, flowv1.Flow_SetEventRuleOverride_FullMethodName, func(args mock.Arguments) {
					var request flowv1.SetEventRuleOverrideRequest
					testFlowProxyRequest(t, args, &request)
					require.Equal(t, leakageEventType, request.GetEventType())
					require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, request.GetScope().GetType())
					require.Len(t, request.GetActions(), 1)
					if test.wantTarget == nil {
						require.NotNil(t, request.GetActions()[0].GetNoop())
					} else {
						require.Equal(t, test.wantStrategy, request.GetActions()[0].GetSubmitTask().GetTargetStrategy())
					}
				})
			}
			scp.IDClientMap[site.ID.String()] = mockTC
			handler := NewPutDefaultLeakagePolicyOverrideHandler(dbSession, scp, cfg)
			requestURL := "/v2/org/" + org + "/nico/leakage/policy/default/override"
			if test.querySite {
				requestURL += "?siteId=" + site.ID.String()
			}
			body := strings.ReplaceAll(test.body, "{{siteId}}", site.ID.String())
			request := httptest.NewRequest(http.MethodPut, requestURL, strings.NewReader(body))
			request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			recorder := httptest.NewRecorder()
			ec := e.NewContext(request, recorder)
			ec.SetParamNames("orgName")
			ec.SetParamValues(org)
			ec.Set("user", providerUser)
			require.NoError(t, handler.Handle(ec))
			if test.wantError != "" {
				require.Equal(t, http.StatusBadRequest, recorder.Code)
				var response map[string]any
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Equal(t, test.wantError, response["message"])
				require.Empty(t, mockTC.Calls)
				return
			}

			require.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
			var response model.APILeakagePolicy
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
			require.Equal(t, test.wantTarget, response.TargetStrategy)
			mockTC.AssertExpectations(t)
		})
	}
}

func TestPutRackLeakagePolicyHandler_Handle(t *testing.T) {
	tests := []struct {
		name        string
		body        string
		wantMessage string
		querySite   bool
		success     bool
	}{
		{name: "body Site routes rack override", body: `{"siteId":"{{siteId}}","action":"NoAction","targetStrategy":null,"onConflict":null}`, success: true},
		{name: "query siteId rejected", body: `{"siteId":"{{siteId}}","action":"NoAction","targetStrategy":null,"onConflict":null}`, querySite: true, wantMessage: "Unknown query parameter specified in request: siteId"},
		{name: "wrong field type preserves decoder detail", body: `{"siteId":"{{siteId}}","action":"ForcePowerOff","targetStrategy":"LeakingSource","onConflict":123}`, wantMessage: "invalid onConflict: json: cannot unmarshal number into Go value of type string"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			dbSession := testRackInitDB(t)
			defer dbSession.Close()
			cfg := common.GetTestConfig()
			temporalConfig, _ := cfg.GetTemporalConfig()
			scp := sc.NewClientPool(temporalConfig)
			org := "test-org"
			_, site, _ := testRackSetupTestData(t, dbSession, org)
			user := testRackBuildUser(t, dbSession, "provider-user-leakage-rack-policy-put", org, []string{authz.ProviderAdminRole})
			mockTC := &tmocks.Client{}
			if test.success {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("test-workflow-id")
				action := &flowv1.EventRuleAction{Name: "record", Spec: &flowv1.EventRuleAction_Noop{Noop: &flowv1.EventRuleNoopAction{Reason: "record"}}}
				testFlowProxyReply(t, run, &flowv1.EventRule{EventType: leakageEventType, Enabled: true, Actions: []*flowv1.EventRuleAction{action}})
				testFlowProxyMethodDispatch(t, mockTC, run, flowv1.Flow_SetEventRuleOverride_FullMethodName, func(args mock.Arguments) {
					var request flowv1.SetEventRuleOverrideRequest
					testFlowProxyRequest(t, args, &request)
					require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, request.GetScope().GetType())
					require.Equal(t, "rack-17", request.GetScope().GetRackExternalId())
					require.NotNil(t, request.GetActions()[0].GetNoop())
				})
			}
			scp.IDClientMap[site.ID.String()] = mockTC
			e := echo.New()
			requestURL := "/v2/org/" + org + "/nico/rack/rack-17/leakage/policy"
			if test.querySite {
				requestURL += "?siteId=" + site.ID.String()
			}
			body := strings.ReplaceAll(test.body, "{{siteId}}", site.ID.String())
			request := httptest.NewRequest(http.MethodPut, requestURL, strings.NewReader(body))
			request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			recorder := httptest.NewRecorder()
			ec := e.NewContext(request, recorder)
			ec.SetParamNames("orgName", "rackId")
			ec.SetParamValues(org, "rack-17")
			ec.Set("user", user)
			handler := NewPutRackLeakagePolicyHandler(dbSession, scp, cfg)
			require.NoError(t, handler.Handle(ec))
			if test.success {
				require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
				var response model.APILeakagePolicy
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Equal(t, model.LeakageActionNoAction, response.Action)
				mockTC.AssertExpectations(t)
				return
			}
			require.Equal(t, http.StatusBadRequest, recorder.Code)
			var response map[string]any
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
			require.Equal(t, test.wantMessage, response["message"])
			require.Empty(t, mockTC.Calls)
		})
	}
}

func TestGetLeakageHandlingHandler_Handle(t *testing.T) {
	tests := []struct {
		name          string
		status        flowv1.EventHandlingStatus
		reason        flowv1.EventHandlingReason
		message       string
		partial       bool
		shutdown      bool
		missingPolicy bool
		wantState     string
		wantReason    any
		wantStatus    int
		params        string
		wantOffset    int32
		pageNumber    int
		pageSize      int
		empty         bool
	}{
		{name: "captured no action policy", status: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION, message: "No action was required by the applied policy.", wantState: "None", wantStatus: http.StatusOK},
		{name: "failed before creating Tasks", shutdown: true, status: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED, reason: flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ACTION_FAILED, message: "One or more handling actions could not be completed.", wantState: "Failed", wantReason: "ActionFailed", wantStatus: http.StatusOK},
		{name: "retrying without Tasks", shutdown: true, status: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING, reason: flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_FAILED, message: "A handling action failed; another attempt is scheduled.", wantState: "Retrying", wantReason: "AttemptFailed", wantStatus: http.StatusOK},
		{name: "partial failure preserves completed Task status", shutdown: true, status: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED, reason: flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ACTION_FAILED, message: "One or more handling actions could not be completed.", partial: true, wantState: "Failed", wantReason: "ActionFailed", wantStatus: http.StatusOK},
		{name: "unspecified Flow result is Unknown despite no action policy", wantState: "Unknown", wantStatus: http.StatusOK},
		{name: "page number and size map to offset and limit", params: "&pageNumber=3&pageSize=5&orderBy=HANDLED_AT_DESC", wantOffset: 10, pageNumber: 3, pageSize: 5, wantState: "Unknown", wantStatus: http.StatusOK},
		{name: "empty page preserves total and array shape", params: "&pageNumber=100", wantOffset: 99, pageNumber: 100, empty: true, wantStatus: http.StatusOK},
		{name: "zero page number", params: "&pageNumber=0", wantStatus: http.StatusBadRequest},
		{name: "zero page size", params: "&pageSize=0", wantStatus: http.StatusBadRequest},
		{name: "oversized page", params: "&pageSize=101", wantStatus: http.StatusBadRequest},
		{name: "unsupported ordering", params: "&orderBy=HANDLED_AT_ASC", wantStatus: http.StatusBadRequest},
		{name: "cursor parameter removed", params: "&cursor=old-cursor", wantStatus: http.StatusBadRequest},
		{name: "missing historical policy", missingPolicy: true, wantStatus: http.StatusInternalServerError},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.pageNumber == 0 {
				test.pageNumber = 1
			}
			if test.pageSize == 0 {
				test.pageSize = 1
			}
			dbSession := testRackInitDB(t)
			defer dbSession.Close()
			cfg := common.GetTestConfig()
			temporalConfig, _ := cfg.GetTemporalConfig()
			scp := sc.NewClientPool(temporalConfig)
			org := "test-org"
			_, site, _ := testRackSetupTestData(t, dbSession, org)
			user := testRackBuildUser(t, dbSession, "provider-user-leakage-handling", org, []string{authz.ProviderAdminRole})
			item := &flowv1.EventHandling{
				Id:             &flowv1.UUID{Id: "95de143d-f3e7-4650-987d-d655198cf869"},
				EventType:      leakageEventType,
				HandlingStatus: test.status, Reason: test.reason, Message: test.message,
				EventSummary:         "Leak detected on component-17",
				EventFirstObservedAt: timestamppb.New(time.Date(2026, 10, 26, 15, 59, 0, 0, time.UTC)),
				ResourceId:           "component-17",
				ResourceType:         flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT,
				AppliedPolicy:        &flowv1.EventRulePolicy{Actions: (model.APIPutLeakagePolicyRequest{SiteID: site.ID.String(), Action: model.LeakageActionNoAction}).EventRuleActions()},
				HandledAt:            timestamppb.New(time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC)),
			}
			if test.shutdown {
				item.AppliedPolicy = &flowv1.EventRulePolicy{Actions: (model.APIPutLeakagePolicyRequest{SiteID: site.ID.String(), Action: model.LeakageActionForcePowerOff, TargetStrategy: stringPointer(model.LeakageTargetAffected), OnConflict: stringPointer(model.LeakageConflictQueue)}).EventRuleActions()}
			}
			if test.partial {
				item.Tasks = []*flowv1.EventHandlingTask{{Id: &flowv1.UUID{Id: "83759fb2-fbae-469d-b389-199d292485a8"}, Status: flowv1.TaskStatus_TASK_STATUS_COMPLETED}}
			}
			if test.missingPolicy {
				item.AppliedPolicy = nil
			}
			mockTC := &tmocks.Client{}
			if test.wantStatus != http.StatusBadRequest {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("test-workflow-id")
				handlings := []*flowv1.EventHandling{item}
				if test.empty {
					handlings = nil
				}
				testFlowProxyReply(t, run, &flowv1.GetEventHandlingResponse{Handlings: handlings, TotalCount: 17})
				testFlowProxyMethodDispatch(t, mockTC, run, flowv1.Flow_GetEventHandling_FullMethodName, func(args mock.Arguments) {
					var request flowv1.GetEventHandlingRequest
					testFlowProxyRequest(t, args, &request)
					require.Equal(t, leakageEventType, request.GetEventType())
					require.Equal(t, "component-17", request.GetResourceId())
					require.Equal(t, flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, request.GetResourceType())
					require.Equal(t, int32(test.pageSize), request.GetLimit())
					require.Equal(t, test.wantOffset, request.GetOffset())
				})
			}
			scp.IDClientMap[site.ID.String()] = mockTC
			e := echo.New()
			recorder := httptest.NewRecorder()
			request := httptest.NewRequest(http.MethodGet, "/v2/org/"+org+"/nico/leakage/handling?siteId="+site.ID.String()+"&sourceType=Component&sourceId=component-17"+test.params, nil)
			ec := e.NewContext(request, recorder)
			ec.SetParamNames("orgName")
			ec.SetParamValues(org)
			ec.Set("user", user)
			handler := NewGetLeakageHandlingHandler(dbSession, scp, cfg)
			require.NoError(t, handler.Handle(ec))
			require.Equal(t, test.wantStatus, recorder.Code, "body=%s", recorder.Body.String())
			if test.wantStatus == http.StatusBadRequest {
				require.Empty(t, mockTC.Calls)
				return
			}
			if test.wantStatus == http.StatusOK {
				var header pagination.PageResponse
				require.NoError(t, json.Unmarshal([]byte(recorder.Header().Get(pagination.ResponseHeaderName)), &header))
				require.Equal(t, pagination.PageResponse{PageNumber: test.pageNumber, PageSize: test.pageSize, Total: 17, OrderBy: stringPointer("HANDLED_AT_DESC")}, header)
			}

			if test.missingPolicy {
				var response map[string]any
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Equal(t, "Failed to render leakage handling", response["message"])
			} else {
				var items []map[string]any
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &items))
				if test.empty {
					require.JSONEq(t, "[]", recorder.Body.String())
					mockTC.AssertExpectations(t)
					return
				}
				require.Len(t, items, 1)
				result := items[0]
				require.Equal(t, test.wantState, result["handlingStatus"])
				require.Equal(t, test.wantReason, result["reason"])
				require.Equal(t, test.message, result["message"])
				require.NotContains(t, result, "execution")
				require.NotContains(t, result, "taskIds")
				require.Equal(t, "2026-10-26T15:59:00Z", result["event"].(map[string]any)["detectedAt"])
				require.NotContains(t, result, "id")
				require.IsType(t, []any{}, result["tasks"])
				if test.partial {
					require.Equal(t, []any{map[string]any{"id": "83759fb2-fbae-469d-b389-199d292485a8", "status": "Succeeded"}}, result["tasks"])
				} else {
					require.Empty(t, result["tasks"])
				}
			}
			mockTC.AssertExpectations(t)
		})
	}
}

func TestDecodeLeakagePolicyRequest(t *testing.T) {
	tests := []struct {
		name    string
		body    string
		want    model.APIPutLeakagePolicyRequest
		wantErr bool
	}{
		{
			name: "source policy",
			body: `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"ForcePowerOff","targetStrategy":"LeakingSource","onConflict":"Queue"}`,
			want: model.APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: model.LeakageActionForcePowerOff, TargetStrategy: stringPointer(model.LeakageTargetSource), OnConflict: stringPointer(model.LeakageConflictQueue)},
		},
		{
			name: "force power off",
			body: `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"ForcePowerOff","targetStrategy":"AffectedComponents","onConflict":"Queue"}`,
			want: model.APIPutLeakagePolicyRequest{
				SiteID:         "550e8400-e29b-41d4-a716-446655440000",
				Action:         model.LeakageActionForcePowerOff,
				TargetStrategy: stringPointer(model.LeakageTargetAffected),
				OnConflict:     stringPointer(model.LeakageConflictQueue),
			},
		},
		{
			name: "explicit no action nulls",
			body: `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"NoAction","targetStrategy":null,"onConflict":null}`,
			want: model.APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: model.LeakageActionNoAction},
		},
		{name: "missing siteId", body: `{"action":"NoAction","targetStrategy":null,"onConflict":null}`, wantErr: true},
		{name: "wrong siteId JSON type", body: `{"siteId":42,"action":"NoAction","targetStrategy":null,"onConflict":null}`, wantErr: true},
		{
			name:    "old target field",
			body:    `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"ForcePowerOff","target":"AffectedComponents","onConflict":"Queue"}`,
			wantErr: true,
		},
		{
			name:    "omitted nullable field",
			body:    `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"NoAction","targetStrategy":null}`,
			wantErr: true,
		},
		{
			name:    "unknown field",
			body:    `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"NoAction","targetStrategy":null,"onConflict":null,"enabled":false}`,
			wantErr: true,
		},
		{
			name:    "trailing JSON",
			body:    `{"siteId":"550e8400-e29b-41d4-a716-446655440000","action":"NoAction","targetStrategy":null,"onConflict":null} {}`,
			wantErr: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			e := echo.New()
			request := httptest.NewRequest("PUT", "/", strings.NewReader(test.body))
			var got model.APIPutLeakagePolicyRequest
			err := decodeLeakagePolicyRequest(e.NewContext(request, httptest.NewRecorder()), &got)
			if test.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, got)
		})
	}
}

func TestLeakageHandlingWorkflowID(t *testing.T) {
	for _, test := range []struct {
		name     string
		request  *flowv1.GetEventHandlingRequest
		wantSame bool
	}{
		{name: "equivalent normalized request", request: &flowv1.GetEventHandlingRequest{EventType: leakageEventType, ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, ResourceId: "component-a", Limit: 1}, wantSame: true},
		{name: "resource kind", request: &flowv1.GetEventHandlingRequest{EventType: leakageEventType, ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK, ResourceId: "component-a", Limit: 1}},
		{name: "resource ID", request: &flowv1.GetEventHandlingRequest{EventType: leakageEventType, ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, ResourceId: "component-b", Limit: 1}},
		{name: "page size", request: &flowv1.GetEventHandlingRequest{EventType: leakageEventType, ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, ResourceId: "component-a", Limit: 2}},
		{name: "page offset", request: &flowv1.GetEventHandlingRequest{EventType: leakageEventType, ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, ResourceId: "component-a", Limit: 1, Offset: 1}},
	} {
		t.Run(test.name, func(t *testing.T) {
			base := &flowv1.GetEventHandlingRequest{EventType: leakageEventType, ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, ResourceId: "component-a", Limit: 1}
			got := leakageHandlingWorkflowID(test.request)
			if test.wantSame {
				require.Equal(t, leakageHandlingWorkflowID(base), got)
			} else {
				require.NotEqual(t, leakageHandlingWorkflowID(base), got)
			}
		})
	}
}

func stringPointer(value string) *string { return &value }

func TestGetAllLeakagePolicyHandler_Handle(t *testing.T) {
	for _, test := range []struct {
		name, params, role, userOrg, siteValue                                    string
		pageNumber, pageSize, offset                                              int
		empty, malformed, missingUser, missingSite, foreignSite, noFlow, omitSite bool
		wantStatus                                                                int
	}{
		{name: "effective default and Rack metadata", pageNumber: 1, pageSize: 20, wantStatus: http.StatusOK},
		{name: "normalized pagination", params: "&pageNumber=2&pageSize=1&orderBy=SCOPE_ASC", pageNumber: 2, pageSize: 1, offset: 1, wantStatus: http.StatusOK},
		{name: "empty page retains total", params: "&pageNumber=5&pageSize=1", pageNumber: 5, pageSize: 1, offset: 4, empty: true, wantStatus: http.StatusOK},
		{name: "unrepresentable policy is not silently omitted", malformed: true, pageNumber: 1, pageSize: 20, wantStatus: http.StatusInternalServerError},
		{name: "unknown selector", params: "&enabled=false", wantStatus: http.StatusBadRequest},
		{name: "zero page number", params: "&pageNumber=0", wantStatus: http.StatusBadRequest},
		{name: "oversized page", params: "&pageSize=101", wantStatus: http.StatusBadRequest},
		{name: "unsupported ordering", params: "&orderBy=SCOPE_DESC", wantStatus: http.StatusBadRequest},
		{name: "missing user", missingUser: true, wantStatus: http.StatusInternalServerError},
		{name: "tenant cannot read Site policy", role: authz.TenantAdminRole, wantStatus: http.StatusForbidden},
		{name: "Site query must exist", missingSite: true, wantStatus: http.StatusBadRequest},
		{name: "missing Site selector", omitSite: true, wantStatus: http.StatusBadRequest},
		{name: "invalid Site UUID", siteValue: "not-a-uuid", wantStatus: http.StatusBadRequest},
		{name: "user must belong to request org", userOrg: "other-org", wantStatus: http.StatusForbidden},
		{name: "Site must belong to current Provider", foreignSite: true, wantStatus: http.StatusForbidden},
		{name: "Flow must be enabled", noFlow: true, wantStatus: http.StatusPreconditionFailed},
	} {
		t.Run(test.name, func(t *testing.T) {
			dbSession := testRackInitDB(t)
			defer dbSession.Close()
			cfg := common.GetTestConfig()
			temporalConfig, _ := cfg.GetTemporalConfig()
			scp := sc.NewClientPool(temporalConfig)
			org := "test-org"
			_, site, _ := testRackSetupTestData(t, dbSession, org)
			if test.foreignSite {
				foreignProvider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "foreign-provider", Org: "other-org"}
				_, err := dbSession.DB.NewInsert().Model(foreignProvider).Exec(t.Context())
				require.NoError(t, err)
				site.InfrastructureProviderID = foreignProvider.ID
				_, err = dbSession.DB.NewUpdate().Model(site).Column("infrastructure_provider_id").WherePK().Exec(t.Context())
				require.NoError(t, err)
			}
			if test.noFlow {
				site.Config = &cdbm.SiteConfig{}
				_, err := dbSession.DB.NewUpdate().Model(site).Column("config").WherePK().Exec(t.Context())
				require.NoError(t, err)
			}
			role := test.role
			if role == "" {
				role = authz.ProviderAdminRole
			}
			userOrg := org
			if test.userOrg != "" {
				userOrg = test.userOrg
			}
			user := testRackBuildUser(t, dbSession, "leakage-policy-list-user", userOrg, []string{role})
			mockTC := &tmocks.Client{}
			if test.wantStatus == http.StatusOK || test.malformed {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("test-workflow-id")
				actions := (model.APIPutLeakagePolicyRequest{Action: model.LeakageActionNoAction}).EventRuleActions()
				defaultActions := (model.APIPutLeakagePolicyRequest{Action: model.LeakageActionForcePowerOff, TargetStrategy: stringPointer(model.LeakageTargetAffected), OnConflict: stringPointer(model.LeakageConflictQueue)}).EventRuleActions()
				policies := []*flowv1.ScopedEventRule{
					{ScopeType: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, Rule: &flowv1.EventRule{EventType: leakageEventType, Enabled: true, ReadOnly: true, Actions: defaultActions}},
					{ScopeType: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackId: stringPointer("rack-a"), Rule: &flowv1.EventRule{EventType: leakageEventType, Enabled: true, Actions: actions}},
				}
				if test.empty {
					policies = nil
				}
				if test.malformed {
					policies[1].Rule.Enabled = false
				}
				policies = policies[min(test.offset, len(policies)):min(test.offset+test.pageSize, len(policies))]
				testFlowProxyReply(t, run, &flowv1.ListScopedEventRulesResponse{Policies: policies, Total: 2})
				testFlowProxyMethodDispatch(t, mockTC, run, flowv1.Flow_ListScopedEventRules_FullMethodName, func(args mock.Arguments) {
					var request flowv1.ListScopedEventRulesRequest
					testFlowProxyRequest(t, args, &request)
					require.Equal(t, leakageEventType, request.GetEventType())
					require.NotNil(t, request.GetPagination())
					require.EqualValues(t, test.offset, request.GetPagination().GetOffset())
					require.EqualValues(t, test.pageSize, request.GetPagination().GetLimit())
					options := args.Get(1).(tClient.StartWorkflowOptions)
					require.Equal(t, "leakage-policy-list-"+common.RequestHash(&request), options.ID)
					require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
				})
			}
			scp.IDClientMap[site.ID.String()] = mockTC
			siteID := site.ID.String()
			if test.siteValue != "" {
				siteID = test.siteValue
			}
			if test.omitSite {
				siteID = ""
			}
			if test.missingSite {
				siteID = "11111111-1111-4111-8111-111111111111"
			}
			request := httptest.NewRequest(http.MethodGet, "/v2/org/"+org+"/nico/leakage/policy?siteId="+siteID+test.params, nil)
			recorder := httptest.NewRecorder()
			c := echo.New().NewContext(request, recorder)
			c.SetParamNames("orgName")
			c.SetParamValues(org)
			if !test.missingUser {
				c.Set("user", user)
			}
			require.NoError(t, NewGetAllLeakagePolicyHandler(dbSession, scp, cfg).Handle(c))
			require.Equal(t, test.wantStatus, recorder.Code, "body=%s", recorder.Body.String())
			if test.wantStatus != http.StatusOK {
				if !test.malformed {
					require.Empty(t, mockTC.Calls)
				}
				return
			}
			var page pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(recorder.Header().Get(pagination.ResponseHeaderName)), &page))
			require.Equal(t, pagination.PageResponse{PageNumber: test.pageNumber, PageSize: test.pageSize, Total: 2, OrderBy: stringPointer("SCOPE_ASC")}, page)
			if test.empty {
				require.JSONEq(t, `[]`, recorder.Body.String())
				return
			}
			var rows []model.APILeakageScopedPolicy
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &rows))
			if test.offset == 0 {
				require.Len(t, rows, 2)
				require.Equal(t, site.ID.String(), rows[0].SiteID)
				require.Equal(t, model.LeakagePolicyScopeDefault, rows[0].Scope)
				require.Equal(t, model.LeakagePolicySourceBuiltIn, rows[0].Source)
				require.Nil(t, rows[0].RackID)
				require.Equal(t, model.LeakageActionForcePowerOff, rows[0].Policy.Action)
				require.Equal(t, stringPointer(model.LeakageTargetAffected), rows[0].Policy.TargetStrategy)
			} else {
				require.Len(t, rows, 1)
			}
			rack := rows[len(rows)-1]
			require.Equal(t, site.ID.String(), rack.SiteID)
			require.Equal(t, model.LeakagePolicyScopeRack, rack.Scope)
			require.Equal(t, model.LeakagePolicySourceOverride, rack.Source)
			require.Equal(t, stringPointer("rack-a"), rack.RackID)
			require.Equal(t, model.LeakageActionNoAction, rack.Policy.Action)
			mockTC.AssertExpectations(t)
		})
	}
}

func TestGetDefaultLeakagePolicyHandler_Handle(t *testing.T) {
	for _, builtIn := range []bool{true, false} {
		name := "Site override"
		if builtIn {
			name = "built-in fallback"
		}
		t.Run(name, func(t *testing.T) {
			dbSession := testRackInitDB(t)
			defer dbSession.Close()
			cfg := common.GetTestConfig()
			temporalConfig, _ := cfg.GetTemporalConfig()
			scp := sc.NewClientPool(temporalConfig)
			_, site, _ := testRackSetupTestData(t, dbSession, "test-org")
			user := testRackBuildUser(t, dbSession, "leakage-default-reader", "test-org", []string{authz.ProviderAdminRole})
			mockTC, run := &tmocks.Client{}, &tmocks.WorkflowRun{}
			run.On("GetID").Return("test-workflow-id")
			testFlowProxyReply(t, run, &flowv1.EventRule{EventType: leakageEventType, Enabled: true, ReadOnly: builtIn, Actions: (model.APIPutLeakagePolicyRequest{Action: model.LeakageActionNoAction}).EventRuleActions()})
			testFlowProxyMethodDispatch(t, mockTC, run, flowv1.Flow_GetEffectiveEventRuleForScope_FullMethodName, func(args mock.Arguments) {
				var request flowv1.GetEffectiveEventRuleForScopeRequest
				testFlowProxyRequest(t, args, &request)
				require.Equal(t, leakageEventType, request.GetEventType())
				require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, request.GetScope().GetType())
			})
			scp.IDClientMap[site.ID.String()] = mockTC
			request := httptest.NewRequest(http.MethodGet, "/v2/org/test-org/nico/leakage/policy/default?siteId="+site.ID.String(), nil)
			recorder := httptest.NewRecorder()
			c := echo.New().NewContext(request, recorder)
			c.SetParamNames("orgName")
			c.SetParamValues("test-org")
			c.Set("user", user)
			require.NoError(t, NewGetDefaultLeakagePolicyHandler(dbSession, scp, cfg).Handle(c))
			require.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
			var policy model.APILeakagePolicy
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &policy))
			require.Equal(t, model.LeakageActionNoAction, policy.Action)
			mockTC.AssertExpectations(t)
		})
	}
}

func TestDeleteDefaultLeakagePolicyOverrideHandler_Handle(t *testing.T) {
	dbSession := testRackInitDB(t)
	defer dbSession.Close()
	cfg := common.GetTestConfig()
	temporalConfig, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(temporalConfig)
	_, site, _ := testRackSetupTestData(t, dbSession, "test-org")
	user := testRackBuildUser(t, dbSession, "leakage-default-delete", "test-org", []string{authz.ProviderAdminRole})
	mockTC, run := &tmocks.Client{}, &tmocks.WorkflowRun{}
	run.On("GetID").Return("test-workflow-id")
	testFlowProxyReply(t, run, &emptypb.Empty{})
	testFlowProxyMethodDispatch(t, mockTC, run, flowv1.Flow_DeleteEventRuleOverride_FullMethodName, func(args mock.Arguments) {
		var request flowv1.DeleteEventRuleOverrideRequest
		testFlowProxyRequest(t, args, &request)
		require.Equal(t, leakageEventType, request.GetEventType())
		require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, request.GetScope().GetType())
	})
	scp.IDClientMap[site.ID.String()] = mockTC
	request := httptest.NewRequest(http.MethodDelete, "/v2/org/test-org/nico/leakage/policy/default/override?siteId="+site.ID.String(), nil)
	recorder := httptest.NewRecorder()
	c := echo.New().NewContext(request, recorder)
	c.SetParamNames("orgName")
	c.SetParamValues("test-org")
	c.Set("user", user)
	require.NoError(t, NewDeleteDefaultLeakagePolicyOverrideHandler(dbSession, scp, cfg).Handle(c))
	require.Equal(t, http.StatusNoContent, recorder.Code, "body=%s", recorder.Body.String())
	require.Empty(t, recorder.Body.String())
	mockTC.AssertExpectations(t)
}
