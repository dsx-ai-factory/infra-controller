// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestGetAllEventPolicyHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "catalog bound false", verb: http.MethodGet, query: "&bound=false&pageNumber=2&pageSize=1", code: 200, method: flowv1.Flow_ListEventRules_FullMethodName, hashPrefix: "event-list-policies-", reply: &flowv1.ListEventRulesResponse{Rules: []*flowv1.EventRule{eventHandlerTestPolicy()}, Total: 3}, request: &flowv1.ListEventRulesRequest{}, list: true},
		{name: "unsupported sort", verb: http.MethodGet, query: "&orderBy=NAME_DESC", code: 400, list: true},
		{name: "missing user", verb: http.MethodGet, noUser: true, code: 500, list: true},
		{name: "requires provider admin", verb: http.MethodGet, noRole: true, code: 403, list: true},
		{name: "storage failure is private", verb: http.MethodGet, code: 500, method: flowv1.Flow_ListEventRules_FullMethodName, list: true, proxyErr: temporal.NewApplicationError("SQLSTATE 42P01: relation event_rules does not exist", "StorageFailure"), wantMessage: "Failed to process event request"},
		{name: "policy ID belongs in the resource path", verb: http.MethodGet, query: "&policyId=POLICY", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetAllEventPolicyHandler(db, pool, cfg).Handle
	})
}

func TestCreateEventPolicyHandler_Handle(t *testing.T) {
	pinnedPolicy := eventHandlerTestPolicy()
	operationRuleID := "63f1c140-81c9-4711-8c3a-1664aaed2cd9"
	pinnedPolicy.Actions = []*flowv1.EventRuleAction{{Name: "shutdown", Spec: &flowv1.EventRuleAction_SubmitTask{SubmitTask: &flowv1.EventRuleSubmitTaskAction{
		TargetStrategy:   flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE,
		ConflictStrategy: flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE,
		Operation:        &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: operationRuleID}, Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF}}},
	}}}}
	tests := []eventHandlerTestCase{
		{name: "create enabled", verb: http.MethodPost, body: `{"siteId":"SITE","name":"Example","eventType":"hardware.leak.detected","actions":[{"name":"record","noAction":{}}]}`, code: 201, method: flowv1.Flow_CreateEventRule_FullMethodName, reply: eventHandlerTestPolicy(), request: &flowv1.CreateEventRuleRequest{}},
		{name: "create with operation rule", verb: http.MethodPost, body: `{"siteId":"SITE","name":"Example","eventType":"hardware.leak.detected","actions":[{"name":"shutdown","submitTask":{"targetStrategy":"Source","onConflict":"Queue","operation":{"ruleId":"63f1c140-81c9-4711-8c3a-1664aaed2cd9","powerControl":{"operation":"ForcePowerOff"}}}}]}`, code: 201, method: flowv1.Flow_CreateEventRule_FullMethodName, reply: pinnedPolicy, request: &flowv1.CreateEventRuleRequest{}, operationRuleID: operationRuleID},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewCreateEventPolicyHandler(db, pool, cfg).Handle
	})
}

func TestGetEventPolicyHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "get definition", verb: http.MethodGet, code: 200, method: flowv1.Flow_GetEventRule_FullMethodName, reply: eventHandlerTestPolicy()},
		{name: "irrelevant filter", verb: http.MethodGet, query: "&bound=true", code: 400},
		{name: "proxy timeout", verb: http.MethodGet, code: 504, method: flowv1.Flow_GetEventRule_FullMethodName, timeout: true},
		{name: "single resource does not accept pagination", verb: http.MethodGet, query: "&pageSize=1", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetEventPolicyHandler(db, pool, cfg).Handle
	})
}

func TestUpdateEventPolicyHandler_Handle(t *testing.T) {
	disabledPolicy := eventHandlerTestPolicy()
	disabledPolicy.Enabled = false
	tests := []eventHandlerTestCase{
		{name: "enable definition", verb: http.MethodPatch, body: `{"siteId":"SITE","enabled":true}`, code: 200, method: flowv1.Flow_SetEventRuleEnabled_FullMethodName, reply: eventHandlerTestPolicy(), request: &flowv1.SetEventRuleEnabledRequest{}, wantEnabled: true},
		{name: "disable definition", verb: http.MethodPatch, body: `{"siteId":"SITE","enabled":false}`, code: 200, method: flowv1.Flow_SetEventRuleEnabled_FullMethodName, reply: disabledPolicy, request: &flowv1.SetEventRuleEnabledRequest{}},
		{name: "strict patch", verb: http.MethodPatch, body: `{"siteId":"SITE","enabled":true,"name":"changed"}`, code: 400},
		{name: "missing desired state", verb: http.MethodPatch, body: `{"siteId":"SITE"}`, code: 400},
		{name: "site in query with body", verb: http.MethodPatch, body: `{"siteId":"SITE","enabled":true}`, query: "?siteId=SITE", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewUpdateEventPolicyHandler(db, pool, cfg).Handle
	})
}

func TestDeleteEventPolicyHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "delete unbound", verb: http.MethodDelete, code: 204, method: flowv1.Flow_DeleteEventRule_FullMethodName, reply: &emptypb.Empty{}, request: &flowv1.DeleteEventRuleRequest{}},
		{name: "bound policy is a conflict", verb: http.MethodDelete, code: 409, method: flowv1.Flow_DeleteEventRule_FullMethodName, request: &flowv1.DeleteEventRuleRequest{}, proxyErr: temporal.NewNonRetryableApplicationError("policy still has bindings", swe.ErrTypeNICoAlreadyExists, nil)},
		{name: "irrelevant filter", verb: http.MethodDelete, query: "&bound=true", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewDeleteEventPolicyHandler(db, pool, cfg).Handle
	})
}

func TestGetSiteEventPolicyBindingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "read explicit binding", verb: http.MethodGet, query: "&eventType=hardware.leak.detected", code: 200, method: flowv1.Flow_GetEventRuleBinding_FullMethodName, reply: eventHandlerTestBinding(""), request: &flowv1.GetEventRuleBindingRequest{}},
		{name: "missing event type for slot", verb: http.MethodGet, code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetSiteEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestGetRackEventPolicyBindingHandler_Handle(t *testing.T) {
	externalRackID := "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
	tests := []eventHandlerTestCase{
		{name: "UUID-shaped external Rack ID", verb: http.MethodGet, rackID: externalRackID, query: "&eventType=hardware.leak.detected", code: 200, method: flowv1.Flow_GetEventRuleBinding_FullMethodName, reply: eventHandlerTestBinding(externalRackID), request: &flowv1.GetEventRuleBindingRequest{}, hashPrefix: "event-get-rack-binding-"},
		{name: "long Rack identifier uses full digest", verb: http.MethodGet, rackID: strings.Repeat("r", 80), query: "&eventType=hardware.leak.detected", code: 200, method: flowv1.Flow_GetEventRuleBinding_FullMethodName, reply: eventHandlerTestBinding(strings.Repeat("r", 80)), request: &flowv1.GetEventRuleBindingRequest{}, hashPrefix: "event-get-rack-binding-"},
		{name: "read explicit Rack binding", verb: http.MethodGet, rackID: "rack-17", query: "&eventType=hardware.leak.detected", code: 200, method: flowv1.Flow_GetEventRuleBinding_FullMethodName, reply: eventHandlerTestBinding("rack-17"), request: &flowv1.GetEventRuleBindingRequest{}, hashPrefix: "event-get-rack-binding-"},
		{name: "Rack identifier with separators", verb: http.MethodGet, rackID: "rack-17-2:ab", query: "&eventType=hardware.leak.detected", code: 200, method: flowv1.Flow_GetEventRuleBinding_FullMethodName, reply: eventHandlerTestBinding("rack-17-2:ab"), request: &flowv1.GetEventRuleBindingRequest{}, hashPrefix: "event-get-rack-binding-"},
		{name: "missing Rack identifier", verb: http.MethodGet, query: "&eventType=hardware.leak.detected", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetRackEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestPutSiteEventPolicyBindingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "reject Rack selector in Site body", verb: http.MethodPut, body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY","rackId":"rack-17"}`, code: 400},
		{name: "create Site", verb: http.MethodPut, body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY"}`, code: 201, method: flowv1.Flow_PutEventRuleBinding_FullMethodName, reply: &flowv1.PutEventRuleBindingResponse{Binding: eventHandlerTestBinding(""), Created: true}, request: &flowv1.PutEventRuleBindingRequest{}},
		{name: "assign Site", verb: http.MethodPut, body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY"}`, code: 200, method: flowv1.Flow_PutEventRuleBinding_FullMethodName, reply: &flowv1.PutEventRuleBindingResponse{Binding: eventHandlerTestBinding("")}, request: &flowv1.PutEventRuleBindingRequest{}},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewPutSiteEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestPutRackEventPolicyBindingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "short Rack identifier uses full digest", verb: http.MethodPut, rackID: "r17", body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY"}`, code: 200, method: flowv1.Flow_PutEventRuleBinding_FullMethodName, reply: &flowv1.PutEventRuleBindingResponse{Binding: eventHandlerTestBinding("r17")}, request: &flowv1.PutEventRuleBindingRequest{}, hashPrefix: "event-put-rack-binding-"},
		{name: "reject body Rack override", verb: http.MethodPut, rackID: "rack-17", body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY","rackId":"rack-18"}`, code: 400},
		{name: "create Rack", verb: http.MethodPut, rackID: "rack-17", body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY"}`, code: 201, method: flowv1.Flow_PutEventRuleBinding_FullMethodName, hashPrefix: "event-put-rack-binding-", reply: &flowv1.PutEventRuleBindingResponse{Binding: eventHandlerTestBinding("rack-17"), Created: true}, request: &flowv1.PutEventRuleBindingRequest{}},
		{name: "assign Rack", verb: http.MethodPut, rackID: "rack-17", body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY"}`, code: 200, method: flowv1.Flow_PutEventRuleBinding_FullMethodName, hashPrefix: "event-put-rack-binding-", reply: &flowv1.PutEventRuleBindingResponse{Binding: eventHandlerTestBinding("rack-17")}, request: &flowv1.PutEventRuleBindingRequest{}},
		{name: "missing Rack identifier", verb: http.MethodPut, body: `{"siteId":"SITE","eventType":"hardware.leak.detected","policyId":"POLICY"}`, code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewPutRackEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestDeleteSiteEventPolicyBindingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "remove binding", verb: http.MethodDelete, query: "&eventType=hardware.leak.detected", code: 204, method: flowv1.Flow_DeleteEventRuleBinding_FullMethodName, reply: &emptypb.Empty{}, request: &flowv1.DeleteEventRuleBindingRequest{}},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewDeleteSiteEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestDeleteRackEventPolicyBindingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "long Rack selector", verb: http.MethodDelete, rackID: strings.Repeat("r", 50), query: "&eventType=hardware.leak.detected", code: 204, method: flowv1.Flow_DeleteEventRuleBinding_FullMethodName, reply: &emptypb.Empty{}, request: &flowv1.DeleteEventRuleBindingRequest{}, hashPrefix: "event-delete-rack-binding-"},
		{name: "short Rack identifier uses full digest", verb: http.MethodDelete, rackID: "rack-17", query: "&eventType=hardware.leak.detected", code: 204, method: flowv1.Flow_DeleteEventRuleBinding_FullMethodName, reply: &emptypb.Empty{}, request: &flowv1.DeleteEventRuleBindingRequest{}, hashPrefix: "event-delete-rack-binding-"},
		{name: "missing Rack identifier", verb: http.MethodDelete, query: "&eventType=hardware.leak.detected", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewDeleteRackEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestGetAllEventPolicyBindingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "where used", verb: http.MethodGet, query: "&policyId=POLICY", code: 200, method: flowv1.Flow_ListEventRuleBindings_FullMethodName, hashPrefix: "event-list-bindings-", reply: &flowv1.ListEventRuleBindingsResponse{Bindings: []*flowv1.EventRuleBinding{eventHandlerTestBinding("")}, Total: 1}, request: &flowv1.ListEventRuleBindingsRequest{}, list: true},
		{name: "all bindings", verb: http.MethodGet, code: 200, method: flowv1.Flow_ListEventRuleBindings_FullMethodName, hashPrefix: "event-list-bindings-", reply: &flowv1.ListEventRuleBindingsResponse{}, request: &flowv1.ListEventRuleBindingsRequest{}, list: true},
		{name: "Rack filters", verb: http.MethodGet, query: "&eventType=hardware.leak.detected&scopeType=Rack&rackId=rack-17", code: 200, method: flowv1.Flow_ListEventRuleBindings_FullMethodName, hashPrefix: "event-list-bindings-", reply: &flowv1.ListEventRuleBindingsResponse{}, request: &flowv1.ListEventRuleBindingsRequest{}, list: true},
		{name: "incompatible selectors", verb: http.MethodGet, query: "&scopeType=Site&rackId=rack-17", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetAllEventPolicyBindingHandler(db, pool, cfg).Handle
	})
}

func TestGetAllEventHandlingHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "handling all types", verb: http.MethodGet, code: 200, method: flowv1.Flow_ListEventHandlings_FullMethodName, hashPrefix: "event-list-handlings-", reply: &flowv1.ListEventHandlingsResponse{Handlings: []*flowv1.EventHandling{{EventType: model.EventTypeHardwareLeakDetected, ResourceId: "component-17", ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, AppliedPolicy: &flowv1.EventRulePolicy{Actions: []*flowv1.EventRuleAction{eventHandlerTestPolicy().GetActions()[0]}}, HandlingStatus: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED}}}, request: &flowv1.ListEventHandlingsRequest{}, list: true, handling: true},
		{name: "unknown type", verb: http.MethodGet, query: "&eventType=other.event", code: 400, list: true, handling: true},
		{name: "zero page", verb: http.MethodGet, query: "&pageSize=0", code: 400, list: true, handling: true},
		{name: "unpaired source", verb: http.MethodGet, query: "&sourceId=component-17", code: 400, list: true, handling: true},
		{name: "Task activity filter is not a handling selector", verb: http.MethodGet, query: "&activeOnly=true", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetAllEventHandlingHandler(db, pool, cfg).Handle
	})
}

func TestGetAllEventHandlingTaskHandler_Handle(t *testing.T) {
	tests := []eventHandlerTestCase{
		{name: "Task policy filter", verb: http.MethodGet, query: "&policyId=POLICY&activeOnly=true", code: 200, method: flowv1.Flow_ListEventTasks_FullMethodName, hashPrefix: "event-list-tasks-", reply: &flowv1.ListTasksResponse{Tasks: []*flowv1.Task{}}, request: &flowv1.ListEventTasksRequest{}, list: true},
		{name: "exact source filter is not a Task selector", verb: http.MethodGet, query: "&sourceType=Rack&sourceId=rack-17", code: 400},
	}
	runEventHandlerTests(t, tests, func(db *cdb.Session, pool *sc.ClientPool, cfg *config.Config) echo.HandlerFunc {
		return NewGetAllEventHandlingTaskHandler(db, pool, cfg).Handle
	})
}

type eventHandlerTestCase struct {
	operationRuleID string
	hashPrefix      string
	name            string
	verb            string
	body            string
	query           string
	code            int
	method          string
	reply           proto.Message
	request         proto.Message
	noUser          bool
	noRole          bool
	timeout         bool
	proxyErr        error
	wantEnabled     bool
	wantMessage     string
	rackID          string
	list            bool
	handling        bool
}

const eventHandlerTestPolicyID = "550e8400-e29b-41d4-a716-446655440000"

func eventHandlerTestPolicy() *flowv1.EventRule {
	action := &flowv1.EventRuleAction{Name: "record", Spec: &flowv1.EventRuleAction_Noop{Noop: &flowv1.EventRuleNoopAction{}}}
	return &flowv1.EventRule{Id: &flowv1.UUID{Id: eventHandlerTestPolicyID}, Name: "Example", EventType: model.EventTypeHardwareLeakDetected, Enabled: true, Actions: []*flowv1.EventRuleAction{action}}
}

func eventHandlerTestBinding(rackID string) *flowv1.EventRuleBinding {
	binding := &flowv1.EventRuleBinding{RuleId: &flowv1.UUID{Id: eventHandlerTestPolicyID}, EventType: model.EventTypeHardwareLeakDetected, Scope: &flowv1.EventRuleScope{Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE}}
	if rackID != "" {
		binding.Scope = &flowv1.EventRuleScope{Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &flowv1.UUID{Id: "63f1c140-81c9-4711-8c3a-1664aaed2cd9"}}
		binding.RackExternalId = &rackID
	}
	return binding
}

func runEventHandlerTests(t *testing.T, tests []eventHandlerTestCase, newHandler func(*cdb.Session, *sc.ClientPool, *config.Config) echo.HandlerFunc) {
	t.Helper()
	id := eventHandlerTestPolicyID
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			db := testRackInitDB(t)
			defer db.Close()
			cfg := common.GetTestConfig()
			temporalCfg, _ := cfg.GetTemporalConfig()
			pool := sc.NewClientPool(temporalCfg)
			org := "event-test"
			_, site, _ := testRackSetupTestData(t, db, org)
			roles := []string{authz.ProviderAdminRole}
			if tc.noRole {
				roles = []string{authz.ProviderViewerRole}
			}
			user := testRackBuildUser(t, db, "event-user", org, roles)
			client := &tmocks.Client{}
			pool.IDClientMap[site.ID.String()] = client
			var options *tClient.StartWorkflowOptions
			if tc.method != "" {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("event-workflow")
				if tc.timeout {
					run.On("Get", mock.Anything, mock.Anything).Return(temporal.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil))
				} else if tc.proxyErr != nil {
					testFlowProxyFailure(run, tc.proxyErr)
				} else {
					testFlowProxyReply(t, run, tc.reply)
				}
				options = testFlowProxyMethodDispatch(t, client, run, tc.method, func(args mock.Arguments) {
					if tc.request == nil {
						return
					}
					testFlowProxyRequest(t, args, tc.request)
					switch r := tc.request.(type) {
					case *flowv1.CreateEventRuleRequest:
						require.True(t, r.GetEnabled())
						require.Equal(t, model.EventTypeHardwareLeakDetected, r.GetEventType())
						require.Len(t, r.GetActions(), 1)
						if tc.operationRuleID != "" {
							require.Equal(t, tc.operationRuleID, r.GetActions()[0].GetSubmitTask().GetOperation().GetRuleId().GetId())
						}
					case *flowv1.ListEventRulesRequest:
						require.NotNil(t, r.Bound)
						require.False(t, *r.Bound)
						require.EqualValues(t, 1, r.GetPagination().GetOffset())
					case *flowv1.ListEventRuleBindingsRequest:
						if strings.Contains(tc.query, "policyId=") {
							require.Equal(t, id, r.GetRuleId().GetId())
						} else {
							require.Nil(t, r.RuleId)
						}
						if strings.Contains(tc.query, "scopeType=Rack") {
							require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, r.GetScopeType())
							require.Equal(t, "rack-17", r.GetRackId())
							require.Equal(t, model.EventTypeHardwareLeakDetected, r.GetEventType())
						}
					case *flowv1.SetEventRuleEnabledRequest:
						require.Equal(t, id, r.GetRuleId().GetId())
						require.NotNil(t, r.Enabled)
						require.Equal(t, tc.wantEnabled, r.GetEnabled())
					case *flowv1.DeleteEventRuleRequest:
						require.Equal(t, id, r.GetRuleId().GetId())
					case *flowv1.PutEventRuleBindingRequest:
						require.Equal(t, tc.rackID, r.GetScope().GetRackExternalId())
						require.Equal(t, id, r.GetRuleId().GetId())
						if tc.rackID == "" {
							require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, r.GetScope().GetType())
						} else {
							require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, r.GetScope().GetType())
						}
					case *flowv1.GetEventRuleBindingRequest:
						require.Equal(t, tc.rackID, r.GetSelector().GetRackExternalId())
					case *flowv1.DeleteEventRuleBindingRequest:
						require.Equal(t, tc.rackID, r.GetSelector().GetRackExternalId())
					case *flowv1.ListEventHandlingsRequest:
						require.Nil(t, r.GetFilter().EventType)
					case *flowv1.ListEventTasksRequest:
						require.True(t, r.GetActiveOnly())
						require.Equal(t, id, r.GetFilter().GetPolicyId().GetId())
					}
				})
			}
			body := strings.ReplaceAll(strings.ReplaceAll(tc.body, "SITE", site.ID.String()), "POLICY", id)
			query := strings.ReplaceAll(strings.ReplaceAll(tc.query, "SITE", site.ID.String()), "POLICY", id)
			if tc.verb == http.MethodGet || tc.verb == http.MethodDelete {
				query = "?siteId=" + site.ID.String() + query
			}
			req := httptest.NewRequest(tc.verb, "/event"+query, strings.NewReader(body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			recorder := httptest.NewRecorder()
			ctx := echo.New().NewContext(req, recorder)
			ctx.SetParamNames("orgName", "policyId", "rackId")
			ctx.SetParamValues(org, id, tc.rackID)
			if !tc.noUser {
				ctx.Set("user", user)
			}
			require.NoError(t, newHandler(db, pool, cfg)(ctx))
			require.Equal(t, tc.code, recorder.Code, recorder.Body.String())
			if tc.wantMessage != "" {
				var response map[string]any
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Equal(t, tc.wantMessage, response["message"])
			}
			if tc.method == flowv1.Flow_PutEventRuleBinding_FullMethodName && tc.rackID == "" {
				require.Equal(t, "event-put-site-binding-"+model.EventTypeHardwareLeakDetected+"-"+id, options.ID)
				require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
			}
			if tc.hashPrefix != "" {
				require.True(t, strings.HasPrefix(options.ID, tc.hashPrefix))
				digest, err := hex.DecodeString(strings.TrimPrefix(options.ID, tc.hashPrefix))
				require.NoError(t, err)
				require.Len(t, digest, sha256.Size)
				if tc.request != nil {
					expected, err := common.RequestSHA256(tc.request)
					require.NoError(t, err)
					require.Equal(t, tc.hashPrefix+expected, options.ID)
				}
				require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
			}
			if tc.method == flowv1.Flow_DeleteEventRuleBinding_FullMethodName && tc.rackID == "" {
				require.Equal(t, "event-delete-site-binding-"+model.EventTypeHardwareLeakDetected, options.ID)
			}
			if tc.method == flowv1.Flow_GetEventRuleBinding_FullMethodName && tc.rackID == "" {
				require.Equal(t, "event-get-site-binding-"+model.EventTypeHardwareLeakDetected, options.ID)
				require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
			}
			if tc.method == flowv1.Flow_GetEventRule_FullMethodName {
				require.Equal(t, "event-get-policy-"+id, options.ID)
				require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
			}
			if tc.method == flowv1.Flow_SetEventRuleEnabled_FullMethodName {
				require.Equal(t, fmt.Sprintf("event-set-policy-enabled-%s-%t", id, tc.wantEnabled), options.ID)
				var response model.APIEventPolicy
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Equal(t, tc.wantEnabled, response.Enabled)
				require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
			}
			if tc.method == flowv1.Flow_DeleteEventRule_FullMethodName {
				require.Equal(t, "event-delete-policy-"+id, options.ID)
				require.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
			}
			if tc.method == "" {
				require.Empty(t, client.Calls)
			} else {
				client.AssertExpectations(t)
			}
			if tc.operationRuleID != "" {
				var policy model.APIEventPolicy
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &policy))
				require.Equal(t, tc.operationRuleID, *policy.Actions[0].SubmitTask.Operation.RuleID)
			}
			if tc.code == 204 {
				require.Empty(t, recorder.Body.String())
			}
			if tc.code == 200 && tc.list {
				require.NotEmpty(t, recorder.Header().Get(pagination.ResponseHeaderName))
			}
			_, directBinding := tc.reply.(*flowv1.EventRuleBinding)
			_, savedBinding := tc.reply.(*flowv1.PutEventRuleBindingResponse)
			if (directBinding || savedBinding) && (tc.code == http.StatusOK || tc.code == http.StatusCreated) {
				var result model.APIEventPolicyBinding
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &result))
				require.Equal(t, site.ID.String(), result.SiteID)
				require.Equal(t, id, result.PolicyID)
				if tc.rackID == "" {
					require.Equal(t, "Site", result.ScopeType)
					require.Nil(t, result.RackID)
				} else {
					require.Equal(t, "Rack", result.ScopeType)
					require.Equal(t, tc.rackID, *result.RackID)
				}
			}
			if tc.handling && tc.code == 200 {
				var rows []model.APIEventHandling
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &rows))
				require.Equal(t, model.APIEventHandlingStatusFailed, rows[0].HandlingStatus)
				require.NotNil(t, rows[0].Tasks)
			}
		})
	}
}
