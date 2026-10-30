// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestAPIPutLeakagePolicyRequest_Validate(t *testing.T) {
	targetStrategy, affected := LeakageTargetSource, LeakageTargetAffected
	queue := LeakageConflictQueue
	tests := []struct {
		name      string
		request   APIPutLeakagePolicyRequest
		wantError bool
	}{
		{name: "missing siteId", request: APIPutLeakagePolicyRequest{Action: LeakageActionNoAction}, wantError: true},
		{name: "invalid siteId UUID", request: APIPutLeakagePolicyRequest{SiteID: "not-a-uuid", Action: LeakageActionNoAction}, wantError: true},
		{name: "force power off", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, TargetStrategy: &affected, OnConflict: &queue}},
		{name: "no action", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionNoAction}},
		{name: "force missing targetStrategy", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, OnConflict: &queue}, wantError: true},
		{name: "force missing conflict", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, TargetStrategy: &targetStrategy}, wantError: true},
		{name: "force invalid conflict", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, TargetStrategy: &targetStrategy, OnConflict: stringPointer("Ignore")}, wantError: true},
		{name: "no action with targetStrategy", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionNoAction, TargetStrategy: &targetStrategy}, wantError: true},
		{name: "no action with conflict", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionNoAction, OnConflict: &queue}, wantError: true},
		{name: "removed legacy REST value", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, TargetStrategy: stringPointer("LeakingComponent"), OnConflict: &queue}, wantError: true},
		{name: "lowercase source strategy rejected", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, TargetStrategy: stringPointer("leakingSource"), OnConflict: &queue}, wantError: true},
		{name: "unknown action", request: APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: "Disabled"}, wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantError {
				var fieldErrors validation.Errors
				require.ErrorAs(t, err, &fieldErrors)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPILeakagePolicy_FromProto(t *testing.T) {
	now := time.Date(2026, 10, 20, 18, 32, 0, 0, time.UTC)
	tests := []struct {
		name      string
		response  *flowv1.EventRule
		want      APILeakagePolicy
		wantError bool
	}{
		{name: "built in shutdown", response: policyResponse(true, forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE), now), want: APILeakagePolicy{Action: LeakageActionForcePowerOff, TargetStrategy: stringPointer(LeakageTargetAffected), OnConflict: stringPointer(LeakageConflictQueue)}},
		{name: "source reject", response: policyResponse(false, forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT), now), want: APILeakagePolicy{Action: LeakageActionForcePowerOff, TargetStrategy: stringPointer(LeakageTargetSource), OnConflict: stringPointer(LeakageConflictReject), Updated: &now}},
		{name: "no action", response: policyResponse(false, &flowv1.EventRuleAction{Spec: &flowv1.EventRuleAction_Noop{Noop: &flowv1.EventRuleNoopAction{}}}, now), want: APILeakagePolicy{Action: LeakageActionNoAction, Updated: &now}},
		{name: "unsupported action", response: policyResponse(false, &flowv1.EventRuleAction{Spec: &flowv1.EventRuleAction_SendAlert{SendAlert: &flowv1.EventRuleSendAlertAction{}}}, now), wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var got APILeakagePolicy
			err := got.FromProto(test.response)
			if test.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
			encoded, err := json.Marshal(got)
			require.NoError(t, err)
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(encoded, &fields))
			wantStrategy, err := json.Marshal(test.want.TargetStrategy)
			require.NoError(t, err)
			assert.JSONEq(t, string(wantStrategy), string(fields["targetStrategy"]))
			assert.NotContains(t, fields, "target")
		})
	}
}

func TestAPILeakageHandling_FromProto(t *testing.T) {
	handledAt := time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC)
	tests := []struct {
		name      string
		handling  *flowv1.EventHandling
		wantJSON  string
		wantError bool
	}{
		{
			name: "component shutdown",
			handling: &flowv1.EventHandling{
				Id:                   &flowv1.UUID{Id: "event-id"},
				EventType:            leakageEventTypeForTest,
				EventSummary:         "Leak detected on component-17",
				HandlingStatus:       flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RUNNING,
				Message:              "Handling tasks are in progress.",
				EventFirstObservedAt: timestamppb.New(handledAt.Add(-time.Minute)),
				ResourceId:           "component-17",
				ResourceType:         flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT,
				AppliedPolicy:        &flowv1.EventRulePolicy{Actions: []*flowv1.EventRuleAction{forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT)}},
				Tasks: []*flowv1.EventHandlingTask{
					{Id: &flowv1.UUID{Id: "older-task"}, Status: flowv1.TaskStatus_TASK_STATUS_COMPLETED},
					{Id: &flowv1.UUID{Id: "latest-task"}, Status: flowv1.TaskStatus_TASK_STATUS_RUNNING},
				},
				HandledAt: timestamppb.New(handledAt),
			},
			wantJSON: `{"event":{"detectedAt":"2026-10-26T15:59:00Z","summary":"Leak detected on component-17","sourceId":"component-17","sourceType":"Component"},"appliedPolicy":{"action":"ForcePowerOff","targetStrategy":"LeakingSource","onConflict":"Reject"},"handlingStatus":"ShuttingDown","handledAt":"2026-10-26T16:00:00Z","tasks":[{"id":"older-task","status":"Succeeded"},{"id":"latest-task","status":"Running"}],"reason":null,"message":"Handling tasks are in progress."}`,
		},
		{
			name: "rack no action",
			handling: &flowv1.EventHandling{
				Id:             &flowv1.UUID{Id: "rack-event"},
				HandlingStatus: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION,
				Message:        "No action was required by the applied policy.",
				EventType:      leakageEventTypeForTest,
				ResourceId:     "rack-17",
				ResourceType:   flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK,
				AppliedPolicy:  &flowv1.EventRulePolicy{Actions: (APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionNoAction}).EventRuleActions()},
				HandledAt:      timestamppb.New(handledAt),
			},
			wantJSON: `{"event":{"detectedAt":null,"summary":"","sourceId":"rack-17","sourceType":"Rack"},"appliedPolicy":{"action":"NoAction","targetStrategy":null,"onConflict":null},"handlingStatus":"None","handledAt":"2026-10-26T16:00:00Z","tasks":[],"reason":null,"message":"No action was required by the applied policy."}`,
		},
		{
			name: "Flow result is authoritative despite completed task",
			handling: &flowv1.EventHandling{
				HandlingStatus: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED, Reason: flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ACTION_FAILED, Message: "Authoritative summary from Flow.",
				Id: &flowv1.UUID{Id: "event-id"}, EventType: leakageEventTypeForTest, ResourceId: "component-17", ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT,
				AppliedPolicy: &flowv1.EventRulePolicy{Actions: []*flowv1.EventRuleAction{forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT)}},
				Tasks:         []*flowv1.EventHandlingTask{{Id: &flowv1.UUID{Id: "task-id"}, Status: flowv1.TaskStatus_TASK_STATUS_COMPLETED}}, HandledAt: timestamppb.New(handledAt),
			},
			wantJSON: `{"event":{"detectedAt":null,"summary":"","sourceId":"component-17","sourceType":"Component"},"appliedPolicy":{"action":"ForcePowerOff","targetStrategy":"LeakingSource","onConflict":"Reject"},"handlingStatus":"Failed","handledAt":"2026-10-26T16:00:00Z","tasks":[{"id":"task-id","status":"Succeeded"}],"reason":"ActionFailed","message":"Authoritative summary from Flow."}`,
		},
		{name: "unspecified Flow result remains Unknown", handling: &flowv1.EventHandling{
			EventType:     leakageEventTypeForTest,
			AppliedPolicy: &flowv1.EventRulePolicy{Actions: (APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionNoAction}).EventRuleActions()},
			Tasks:         []*flowv1.EventHandlingTask{{Id: &flowv1.UUID{Id: "future-task"}, Status: 999}},
		}, wantJSON: `{"event":{"detectedAt":null,"summary":"","sourceId":"","sourceType":"Unknown"},"appliedPolicy":{"action":"NoAction","targetStrategy":null,"onConflict":null},"handlingStatus":"Unknown","reason":null,"message":"","tasks":[{"id":"future-task","status":"Unknown"}],"handledAt":"0001-01-01T00:00:00Z"}`},
		{name: "missing snapshot", handling: &flowv1.EventHandling{EventType: leakageEventTypeForTest}, wantError: true},
		{name: "unsupported snapshot", handling: &flowv1.EventHandling{EventType: leakageEventTypeForTest, AppliedPolicy: &flowv1.EventRulePolicy{Actions: []*flowv1.EventRuleAction{{Spec: &flowv1.EventRuleAction_SendAlert{SendAlert: &flowv1.EventRuleSendAlertAction{}}}}}}, wantError: true},
		{name: "missing event", wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var got APILeakageHandling
			err := got.FromProto(test.handling)
			if test.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			encoded, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, test.wantJSON, string(encoded))
		})
	}
}

func TestAPILeakageHandlingStatus_FromProto(t *testing.T) {
	for _, test := range []struct {
		value flowv1.EventHandlingStatus
		want  APILeakageHandlingStatus
	}{
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_UNSPECIFIED, APILeakageHandlingStatusUnknown},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION, APILeakageHandlingStatusNone},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_STARTING, APILeakageHandlingStatusStarting},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING, APILeakageHandlingStatusRetrying},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RUNNING, APILeakageHandlingStatusShuttingDown},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_COMPLETED, APILeakageHandlingStatusDown},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_SKIPPED, APILeakageHandlingStatusSkipped},
		{flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED, APILeakageHandlingStatusFailed},
		{999, APILeakageHandlingStatusUnknown},
	} {
		t.Run(test.value.String(), func(t *testing.T) {
			got := APILeakageHandlingStatusDown
			got.FromProto(test.value)
			assert.Equal(t, test.want, got)
		})
	}
}

func TestAPILeakageHandlingReason_FromProto(t *testing.T) {
	for _, test := range []struct {
		value flowv1.EventHandlingReason
		want  APILeakageHandlingReason
	}{
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_NO_TARGETS, APILeakageHandlingReasonNoTargets},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_FAILED, APILeakageHandlingReasonAttemptFailed},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_INTERRUPTED, APILeakageHandlingReasonAttemptInterrupted},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_RETRY_SCHEDULED, APILeakageHandlingReasonRetryScheduled},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ACTION_FAILED, APILeakageHandlingReasonActionFailed},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_TASK_FAILED, APILeakageHandlingReasonTaskFailed},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_INCOMPLETE_STATE, APILeakageHandlingReasonIncompleteState},
		{flowv1.EventHandlingReason_EVENT_HANDLING_REASON_UNSPECIFIED, ""},
		{999, ""},
	} {
		t.Run(test.value.String(), func(t *testing.T) {
			got := APILeakageHandlingReasonTaskFailed
			got.FromProto(test.value)
			assert.Equal(t, test.want, got)
		})
	}
}

func TestAPILeakageHandlingQuery_Validate(t *testing.T) {
	tests := []struct {
		name      string
		query     APILeakageHandlingQuery
		wantError bool
	}{
		{name: "component source", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeComponent, SourceID: "component-17"}},
		{name: "rack page", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeRack, SourceID: "rack-17"}},
		{name: "missing site", query: APILeakageHandlingQuery{}, wantError: true},
		{name: "invalid source type", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: "Site", SourceID: "site-17"}, wantError: true},
		{name: "missing source ID", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeComponent}, wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.query.Validate()
			if test.wantError {
				var fieldErrors validation.Errors
				require.ErrorAs(t, err, &fieldErrors)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPILeakagePolicyQuery_Validate(t *testing.T) {
	for _, test := range []struct {
		name   string
		siteID string
	}{
		{name: "site scope", siteID: "550e8400-e29b-41d4-a716-446655440000"},
		{name: "missing site"},
	} {
		t.Run(test.name, func(t *testing.T) {
			err := (APILeakagePolicyQuery{SiteID: test.siteID}).Validate()
			if test.siteID == "" {
				var fieldErrors validation.Errors
				require.ErrorAs(t, err, &fieldErrors)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPILeakageHandlingQuery_ToProto(t *testing.T) {
	for _, test := range []struct {
		name       string
		pageNumber int
		pageSize   int
		sourceType string
		wantType   flowv1.EventResourceType
		wantOffset int32
	}{
		{name: "latest component record", pageNumber: 1, pageSize: 1, sourceType: LeakageHandlingSourceTypeComponent, wantType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT},
		{name: "later rack page", pageNumber: 3, pageSize: 5, sourceType: LeakageHandlingSourceTypeRack, wantType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK, wantOffset: 10},
	} {
		t.Run(test.name, func(t *testing.T) {
			page := pagination.PageRequest{PageNumber: &test.pageNumber, PageSize: &test.pageSize}
			require.NoError(t, page.Validate(nil))
			query := APILeakageHandlingQuery{SiteID: "site-id", SourceType: test.sourceType, SourceID: "source-17"}
			require.NoError(t, query.Validate())
			got := query.ToProto(page)
			assert.Equal(t, string(flowtypes.EventTypeHardwareLeakDetected), got.GetEventType())
			assert.Equal(t, "source-17", got.GetResourceId())
			assert.Equal(t, test.wantType, got.GetResourceType())
			assert.Equal(t, test.wantOffset, got.GetOffset())
			assert.Equal(t, int32(test.pageSize), got.GetLimit())
		})
	}
}

func policyResponse(readOnly bool, action *flowv1.EventRuleAction, updated time.Time) *flowv1.EventRule {
	return &flowv1.EventRule{ReadOnly: readOnly, Enabled: true, EventType: leakageEventTypeForTest, Actions: []*flowv1.EventRuleAction{action}, UpdatedAt: timestamppb.New(updated)}
}

const leakageEventTypeForTest = string(flowtypes.EventTypeHardwareLeakDetected)

func forcePowerAction(targetStrategy flowv1.EventRuleTargetStrategy, conflict flowv1.EventRuleConflictStrategy) *flowv1.EventRuleAction {
	return &flowv1.EventRuleAction{Name: "shutdown", Spec: &flowv1.EventRuleAction_SubmitTask{SubmitTask: &flowv1.EventRuleSubmitTaskAction{TargetStrategy: targetStrategy, ConflictStrategy: conflict, Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF}}}}}}
}
func stringPointer(value string) *string { return &value }

func TestAPIPutLeakagePolicyRequest_EventRuleActions(t *testing.T) {
	tests := []struct {
		name   string
		target string
		want   flowv1.EventRuleTargetStrategy
	}{
		{name: "source", target: LeakageTargetSource, want: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE},
		{name: "rack", target: LeakageTargetRack, want: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK},
		{name: "affected components", target: LeakageTargetAffected, want: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			request := APIPutLeakagePolicyRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000", Action: LeakageActionForcePowerOff, TargetStrategy: &test.target, OnConflict: stringPointer(LeakageConflictQueue)}
			require.NoError(t, request.Validate())
			actions := request.EventRuleActions()
			require.Len(t, actions, 1)
			assert.Equal(t, test.want, actions[0].GetSubmitTask().GetTargetStrategy())
		})
	}
}

func TestAPILeakageTargetStrategy_FromProto(t *testing.T) {
	tests := []struct {
		name     string
		strategy flowv1.EventRuleTargetStrategy
		want     APILeakageTargetStrategy
	}{
		{name: "source", strategy: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE, want: "LeakingSource"},
		{name: "affected", strategy: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS, want: "AffectedComponents"},
		{name: "rack", strategy: flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK, want: "LeakingRack"},
		{name: "unknown future strategy", strategy: 99},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := APILeakageTargetStrategy(LeakageTargetSource)
			got.FromProto(test.strategy)
			assert.Equal(t, test.want, got)
			if test.want == "" {
				require.Error(t, got.Validate())
				require.Equal(t, flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_UNSPECIFIED, got.ToProto())
			} else {
				require.NoError(t, got.Validate())
				require.Equal(t, test.strategy, got.ToProto())
			}
		})
	}
}

func TestAPILeakageTargetStrategy_OpenAPISchema(t *testing.T) {
	var document struct {
		Components struct {
			Schemas map[string]struct {
				Enum []string `yaml:"enum"`
			} `yaml:"schemas"`
		} `yaml:"components"`
	}
	require.NoError(t, yaml.Unmarshal(openapi.Spec, &document))
	values := []string{LeakageTargetSource, LeakageTargetRack, LeakageTargetAffected}
	assert.ElementsMatch(t, values, document.Components.Schemas["LeakageTarget"].Enum)
	// A new Flow strategy requires an explicit REST support decision, including
	// whether its old clients, policy readers, and snapshots remain compatible.
	assert.Equal(t, map[string]int32{"EVENT_RULE_TARGET_STRATEGY_UNSPECIFIED": 0, "EVENT_RULE_TARGET_STRATEGY_COMPONENT": 1, "EVENT_RULE_TARGET_STRATEGY_SOURCE": 1, "EVENT_RULE_TARGET_STRATEGY_RACK": 2, "EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS": 3}, flowv1.EventRuleTargetStrategy_value)
}

func TestAPIPutLeakagePolicyRequest_OpenAPISchema(t *testing.T) {
	type parameter struct {
		Name     string `yaml:"name"`
		In       string `yaml:"in"`
		Required bool   `yaml:"required"`
	}
	type operation struct {
		Parameters  []parameter `yaml:"parameters"`
		RequestBody struct {
			Content map[string]struct {
				Schema struct {
					Ref string `yaml:"$ref"`
				} `yaml:"schema"`
			} `yaml:"content"`
		} `yaml:"requestBody"`
	}
	var document struct {
		Paths map[string]struct {
			Parameters []parameter `yaml:"parameters"`
			Get        operation   `yaml:"get"`
			Put        operation   `yaml:"put"`
			Delete     operation   `yaml:"delete"`
		} `yaml:"paths"`
		Components struct {
			Schemas map[string]struct {
				Required   []string `yaml:"required"`
				Properties map[string]struct {
					Type   any    `yaml:"type"`
					Format string `yaml:"format"`
				} `yaml:"properties"`
			} `yaml:"schemas"`
		} `yaml:"components"`
	}
	require.NoError(t, yaml.Unmarshal(openapi.Spec, &document))
	schema := document.Components.Schemas["PutLeakagePolicyRequest"]
	assert.ElementsMatch(t, []string{"siteId", "action", "targetStrategy", "onConflict"}, schema.Required)
	assert.Equal(t, "string", schema.Properties["siteId"].Type)
	assert.Equal(t, "uuid", schema.Properties["siteId"].Format)
	for _, path := range []string{"/v2/org/{org}/nico/leakage/policy/default/override", "/v2/org/{org}/nico/rack/{rackId}/leakage/policy"} {
		t.Run(path, func(t *testing.T) {
			item := document.Paths[path]
			require.Equal(t, "#/components/schemas/PutLeakagePolicyRequest", item.Put.RequestBody.Content["application/json"].Schema.Ref)
			for _, test := range []struct {
				name      string
				operation operation
				wantQuery bool
			}{
				{name: "PUT", operation: item.Put},
				{name: "GET", operation: item.Get, wantQuery: strings.Contains(path, "/rack/")},
				{name: "DELETE", operation: item.Delete, wantQuery: true},
			} {
				t.Run(test.name, func(t *testing.T) {
					params := append([]parameter{}, item.Parameters...)
					params = append(params, test.operation.Parameters...)
					found := false
					for _, param := range params {
						if param.In == "query" && param.Name == "siteId" {
							require.True(t, param.Required)
							found = true
						}
					}
					assert.Equal(t, test.wantQuery, found)
				})
			}
		})
	}
}

func TestAPILeakageScopedPolicy_FromProto(t *testing.T) {
	for _, test := range []struct {
		name               string
		scope              flowv1.EventRuleScopeType
		rackID             *string
		builtIn, wantError bool
		wantScope          APILeakagePolicyScope
		wantSource         APILeakagePolicySource
	}{
		{name: "built-in default", scope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, builtIn: true, wantScope: LeakagePolicyScopeDefault, wantSource: LeakagePolicySourceBuiltIn},
		{name: "default override", scope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, wantScope: LeakagePolicyScopeDefault, wantSource: LeakagePolicySourceOverride},
		{name: "Rack override", scope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, rackID: stringPointer("rack-a"), wantScope: LeakagePolicyScopeRack, wantSource: LeakagePolicySourceOverride},
		{name: "Rack without public ID", scope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, wantScope: LeakagePolicyScopeRack, wantSource: LeakagePolicySourceOverride},
		{name: "default with Rack ID", scope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, rackID: stringPointer("rack-a"), wantError: true},
		{name: "built-in Rack rejected", scope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, builtIn: true, wantError: true},
		{name: "unknown scope", wantError: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			value := &flowv1.ScopedEventRule{ScopeType: test.scope, RackId: test.rackID, Rule: policyResponse(test.builtIn, forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE), time.Now())}
			result := APILeakageScopedPolicy{SiteID: "authorized-site"}
			err := result.FromProto(value)
			if test.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, "authorized-site", result.SiteID)
			require.Equal(t, test.wantScope, result.Scope)
			require.Equal(t, test.wantSource, result.Source)
			require.Equal(t, test.rackID, result.RackID)
			data, err := json.Marshal(result)
			require.NoError(t, err)
			var object map[string]any
			require.NoError(t, json.Unmarshal(data, &object))
			require.NotContains(t, object, "enabled")
			require.Contains(t, object, "rackId")
		})
	}
}
