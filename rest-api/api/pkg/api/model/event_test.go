// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestAPIEventPolicyCreateRequest_Validate(t *testing.T) {
	for _, tc := range []struct {
		name   string
		change func(*APIEventPolicyCreateRequest)
		bad    bool
	}{
		{name: "enabled by default"},
		{name: "explicit staging", change: func(r *APIEventPolicyCreateRequest) { v := false; r.Enabled = &v }},
		{name: "unknown type", change: func(r *APIEventPolicyCreateRequest) { r.EventType = "hardware.temperature.high" }, bad: true},
		{name: "missing site", change: func(r *APIEventPolicyCreateRequest) { r.SiteID = "" }, bad: true},
		{name: "duplicate action names", change: func(r *APIEventPolicyCreateRequest) { r.Actions = append(r.Actions, r.Actions[0]) }, bad: true},
		{name: "empty actions", change: func(r *APIEventPolicyCreateRequest) { r.Actions = APIEventActions{} }, bad: true},
		{name: "multiple specifications", change: func(r *APIEventPolicyCreateRequest) {
			r.Actions[0].SendAlert = &APIEventSendAlert{Severity: "Critical"}
		}, bad: true},
		{name: "empty condition list", change: func(r *APIEventPolicyCreateRequest) {
			r.Actions[0].Condition = &APIEventActionCondition{Severities: []string{}}
		}, bad: true},
		{name: "unknown component type", change: func(r *APIEventPolicyCreateRequest) {
			r.Actions[0].Condition = &APIEventActionCondition{ComponentTypes: []string{"Anything"}}
		}, bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := APIEventPolicyCreateRequest{SiteID: uuid.NewString(), Name: "Policy", EventType: EventTypeHardwareLeakDetected, Actions: APIEventActions{{Name: "record", NoAction: &APIEventNoAction{}}}}
			if tc.change != nil {
				tc.change(&r)
			}
			err := r.Validate()
			if tc.bad {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			converted := r.ToProto()
			require.Equal(t, r.EventType, converted.GetEventType())
			require.NotNil(t, converted.Enabled)
			require.Equal(t, r.Enabled == nil || *r.Enabled, converted.GetEnabled())
		})
	}
}

func TestAPIEventAction_FromProto(t *testing.T) {
	for _, tc := range []struct {
		name   string
		action APIEventAction
	}{
		{name: "no-op", action: APIEventAction{Name: "record", NoAction: &APIEventNoAction{}}},
		{name: "conditional power action", action: APIEventAction{Name: "shutdown", Condition: &APIEventActionCondition{Severities: []string{"Critical"}, ComponentTypes: []string{"Compute", "CDU"}}, SubmitTask: &APIEventSubmitTask{TargetStrategy: "Source", OnConflict: "Queue", Operation: APITaskOperation{PowerControl: &APIPowerControl{Operation: "ForcePowerOff"}}}}},
		{name: "firmware operation", action: APIEventAction{Name: "upgrade", SubmitTask: &APIEventSubmitTask{TargetStrategy: "Rack", OnConflict: "Reject", Operation: APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: "Upgrade", SubTargets: []string{"bmc"}, OverrideVersionCheck: true}}}}},
		{name: "alert", action: APIEventAction{Name: "alert", SendAlert: &APIEventSendAlert{Severity: "Warning", Message: "Observed condition"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			require.NoError(t, tc.action.Validate())
			wire := tc.action.ToProto()
			var got APIEventAction
			require.NoError(t, got.FromProto(wire))
			require.True(t, proto.Equal(wire, got.ToProto()))
			raw, err := json.Marshal(got)
			require.NoError(t, err)
			var fields map[string]any
			require.NoError(t, json.Unmarshal(raw, &fields))
			require.Len(t, fields, 5)
		})
	}
}

func TestAPISiteEventPolicyBindingRequest_Validate(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*APISiteEventPolicyBindingRequest)
		bad    bool
	}{
		{name: "valid Site assignment"},
		{name: "missing Site", change: func(r *APISiteEventPolicyBindingRequest) { r.SiteID = "" }, bad: true},
		{name: "unsupported event type", change: func(r *APISiteEventPolicyBindingRequest) { r.EventType = "other.type" }, bad: true},
		{name: "invalid policy UUID", change: func(r *APISiteEventPolicyBindingRequest) { r.PolicyID = "bad" }, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			request := APISiteEventPolicyBindingRequest{SiteID: uuid.NewString(), EventType: EventTypeHardwareLeakDetected, PolicyID: uuid.NewString()}
			if test.change != nil {
				test.change(&request)
			}
			err := request.Validate()
			if test.bad {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPISiteEventPolicyBindingRequest_ToProto(t *testing.T) {
	request := APISiteEventPolicyBindingRequest{SiteID: uuid.NewString(), EventType: EventTypeHardwareLeakDetected, PolicyID: uuid.NewString()}
	require.NoError(t, request.Validate())
	result := request.ToProto()
	require.Equal(t, request.EventType, result.GetEventType())
	require.Equal(t, request.PolicyID, result.GetRuleId().GetId())
	require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, result.GetScope().GetType())
	require.Nil(t, result.GetScope().GetRackIdentifier())
}

func TestAPIRackEventPolicyBindingRequest_Validate(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*APIRackEventPolicyBindingRequest)
		bad    bool
	}{
		{name: "valid Rack assignment"},
		{name: "missing path Rack", change: func(r *APIRackEventPolicyBindingRequest) { r.RackID = "" }, bad: true},
		{name: "missing Site", change: func(r *APIRackEventPolicyBindingRequest) { r.SiteID = "" }, bad: true},
		{name: "unsupported event type", change: func(r *APIRackEventPolicyBindingRequest) { r.EventType = "other.type" }, bad: true},
		{name: "invalid policy UUID", change: func(r *APIRackEventPolicyBindingRequest) { r.PolicyID = "bad" }, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			request := APIRackEventPolicyBindingRequest{RackID: "rack-17", SiteID: uuid.NewString(), EventType: EventTypeHardwareLeakDetected, PolicyID: uuid.NewString()}
			if test.change != nil {
				test.change(&request)
			}
			err := request.Validate()
			if test.bad {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPIRackEventPolicyBindingRequest_ToProto(t *testing.T) {
	for _, test := range []struct {
		name   string
		rackID string
	}{
		{name: "public Rack", rackID: "rack-17"},
		{name: "UUID-shaped external Rack ID", rackID: uuid.NewString()},
	} {
		t.Run(test.name, func(t *testing.T) {
			request := APIRackEventPolicyBindingRequest{RackID: test.rackID, SiteID: uuid.NewString(), EventType: EventTypeHardwareLeakDetected, PolicyID: uuid.NewString()}
			require.NoError(t, request.Validate())
			result := request.ToProto()
			require.Equal(t, request.EventType, result.GetEventType())
			require.Equal(t, request.PolicyID, result.GetRuleId().GetId())
			require.Equal(t, flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, result.GetScope().GetType())
			require.Equal(t, test.rackID, result.GetScope().GetRackExternalId())
			require.Nil(t, result.GetScope().GetRackId())
			raw, err := json.Marshal(request)
			require.NoError(t, err)
			var body map[string]any
			require.NoError(t, json.Unmarshal(raw, &body))
			require.NotContains(t, body, "rackId")
		})
	}
}

func TestAPIEventPolicyBinding_FromProto(t *testing.T) {
	externalID := uuid.NewString()
	for _, test := range []struct {
		name       string
		scopeType  flowv1.EventRuleScopeType
		externalID string
		wantRackID *string
	}{
		{name: "Site binding", scopeType: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE},
		{name: "UUID-shaped external Rack ID", scopeType: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, externalID: externalID, wantRackID: &externalID},
		{name: "unavailable external Rack ID", scopeType: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK},
	} {
		t.Run(test.name, func(t *testing.T) {
			binding := APIEventPolicyBinding{SiteID: uuid.NewString(), RackID: &externalID}
			source := &flowv1.EventRuleBinding{
				RuleId: &flowv1.UUID{Id: uuid.NewString()}, EventType: EventTypeHardwareLeakDetected,
				Scope: &flowv1.EventRuleScope{Type: test.scopeType},
			}
			if test.scopeType == flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK {
				source.Scope.Id = &flowv1.UUID{Id: uuid.NewString()}
			}
			if test.externalID != "" {
				source.RackExternalId = &test.externalID
			}
			require.NoError(t, binding.FromProto(source))
			require.Equal(t, test.wantRackID, binding.RackID)
			raw, err := json.Marshal(binding)
			require.NoError(t, err)
			var body map[string]any
			require.NoError(t, json.Unmarshal(raw, &body))
			require.Contains(t, body, "rackId")
			if test.wantRackID == nil {
				require.Nil(t, body["rackId"])
			} else {
				require.Equal(t, *test.wantRackID, body["rackId"])
			}
		})
	}
}

func TestAPIEventPolicyUpdateRequest_Validate(t *testing.T) {
	enabled, disabled := true, false
	for _, tc := range []struct {
		name  string
		value *bool
		bad   bool
	}{{"enable", &enabled, false}, {"disable", &disabled, false}, {"missing desired state", nil, true}} {
		t.Run(tc.name, func(t *testing.T) {
			err := (APIEventPolicyUpdateRequest{SiteID: uuid.NewString(), Enabled: tc.value}).Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventPolicyGetRequest_Validate(t *testing.T) {
	for _, tc := range []struct {
		name   string
		siteID string
		bad    bool
	}{
		{name: "valid Site", siteID: uuid.NewString()},
		{name: "missing Site", bad: true},
		{name: "invalid Site", siteID: "not-a-uuid", bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := (APIEventPolicyGetRequest{SiteID: tc.siteID}).Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventPolicyDeleteRequest_Validate(t *testing.T) {
	for _, tc := range []struct {
		name   string
		siteID string
		bad    bool
	}{
		{name: "valid Site", siteID: uuid.NewString()},
		{name: "missing Site", bad: true},
		{name: "invalid Site", siteID: "not-a-uuid", bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := (APIEventPolicyDeleteRequest{SiteID: tc.siteID}).Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventPolicyQuery_Validate(t *testing.T) {
	for _, tc := range []struct {
		name  string
		query APIEventPolicyQuery
		bad   bool
	}{
		{name: "all policies"},
		{name: "selected type", query: APIEventPolicyQuery{EventType: proto.String(EventTypeHardwareLeakDetected)}},
		{name: "explicit false filters", query: APIEventPolicyQuery{Enabled: proto.Bool(false), Bound: proto.Bool(false)}},
		{name: "empty type", query: APIEventPolicyQuery{EventType: proto.String("")}, bad: true},
		{name: "unsupported type", query: APIEventPolicyQuery{EventType: proto.String("other.event")}, bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			q := tc.query
			q.SiteID = uuid.NewString()
			err := q.Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventPolicyQuery_ToProto(t *testing.T) {
	page := eventPageForTest(t)
	for _, test := range []struct {
		name  string
		query APIEventPolicyQuery
		page  *pagination.PageRequest
		want  *flowv1.ListEventRulesRequest
	}{
		{name: "omitted filters", want: &flowv1.ListEventRulesRequest{}},
		{name: "disabled bound policies", query: APIEventPolicyQuery{Enabled: proto.Bool(false), Bound: proto.Bool(true)}, want: &flowv1.ListEventRulesRequest{Enabled: proto.Bool(false), Bound: proto.Bool(true)}},
		{name: "selected enabled unbound policies", query: APIEventPolicyQuery{EventType: proto.String(EventTypeHardwareLeakDetected), Enabled: proto.Bool(true), Bound: proto.Bool(false)}, want: &flowv1.ListEventRulesRequest{EventType: proto.String(EventTypeHardwareLeakDetected), Enabled: proto.Bool(true), Bound: proto.Bool(false)}},
		{name: "paginated catalog", query: APIEventPolicyQuery{Bound: proto.Bool(false)}, page: page, want: &flowv1.ListEventRulesRequest{Bound: proto.Bool(false), Pagination: &flowv1.Pagination{Offset: 20, Limit: 10}}},
	} {
		t.Run(test.name, func(t *testing.T) {
			query := test.query
			query.SiteID = uuid.NewString()
			require.NoError(t, query.Validate())
			require.True(t, proto.Equal(test.want, query.ToProto(test.page)))
		})
	}
}

func TestAPIEventPolicyBindingListRequest_Validate(t *testing.T) {
	for _, test := range []struct {
		name      string
		query     APIEventPolicyBindingListRequest
		bad       bool
		wantScope flowv1.EventRuleScopeType
	}{
		{name: "all assignments"},
		{name: "Site scopes", query: APIEventPolicyBindingListRequest{ScopeType: proto.String("Site")}, wantScope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE},
		{name: "all Rack scopes", query: APIEventPolicyBindingListRequest{ScopeType: proto.String("Rack")}, wantScope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK},
		{name: "combined filters", query: APIEventPolicyBindingListRequest{EventType: proto.String(EventTypeHardwareLeakDetected), ScopeType: proto.String("Rack"), RackID: proto.String("rack-17"), PolicyID: proto.String(uuid.NewString())}, wantScope: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK},
		{name: "unknown type", query: APIEventPolicyBindingListRequest{EventType: proto.String("other.type")}, bad: true},
		{name: "empty event type", query: APIEventPolicyBindingListRequest{EventType: proto.String("")}, bad: true},
		{name: "unknown scope", query: APIEventPolicyBindingListRequest{ScopeType: proto.String("Component")}, bad: true},
		{name: "invalid policy UUID", query: APIEventPolicyBindingListRequest{PolicyID: proto.String("bad")}, bad: true},
		{name: "empty Rack", query: APIEventPolicyBindingListRequest{RackID: proto.String("")}, bad: true},
		{name: "Site with Rack", query: APIEventPolicyBindingListRequest{ScopeType: proto.String("Site"), RackID: proto.String("rack-17")}, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			query := test.query
			query.SiteID = uuid.NewString()
			err := query.Validate()
			if test.bad {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			request := query.ToProto(nil)
			require.Equal(t, query.EventType, request.EventType)
			require.Equal(t, query.RackID, request.RackId)
			if query.PolicyID != nil {
				require.Equal(t, *query.PolicyID, request.GetRuleId().GetId())
			}
			require.Equal(t, test.wantScope, request.GetScopeType())
		})
	}
}

func TestAPIEventPolicyBindingListRequest_ToProto(t *testing.T) {
	page := eventPageForTest(t)
	for _, test := range []struct {
		name     string
		page     *pagination.PageRequest
		wantPage *flowv1.Pagination
	}{
		{name: "without pagination"},
		{name: "with pagination", page: page, wantPage: &flowv1.Pagination{Offset: 20, Limit: 10}},
	} {
		t.Run(test.name, func(t *testing.T) {
			query := APIEventPolicyBindingListRequest{SiteID: uuid.NewString(), EventType: proto.String(EventTypeHardwareLeakDetected), ScopeType: proto.String("Rack"), RackID: proto.String("rack-17"), PolicyID: proto.String(uuid.NewString())}
			require.NoError(t, query.Validate())
			want := &flowv1.ListEventRuleBindingsRequest{EventType: query.EventType, ScopeType: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackId: query.RackID, RuleId: &flowv1.UUID{Id: *query.PolicyID}, Pagination: test.wantPage}
			require.True(t, proto.Equal(want, query.ToProto(test.page)))
		})
	}
}

func TestAPIEventPolicyBindingQuery_Validate(t *testing.T) {
	for _, tc := range []struct {
		name      string
		eventType string
		bad       bool
	}{
		{name: "explicit event type", eventType: EventTypeHardwareLeakDetected},
		{name: "missing event type", bad: true},
		{name: "unsupported type", eventType: "other.event", bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := (APIEventPolicyBindingQuery{SiteID: uuid.NewString(), EventType: tc.eventType}).Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventHandlingQuery_Validate(t *testing.T) {
	for _, tc := range []struct {
		name  string
		query APIEventHandlingQuery
		bad   bool
	}{
		{name: "all handlings"},
		{name: "source pair", query: APIEventHandlingQuery{SourceType: proto.String("Rack"), SourceID: proto.String("rack-17")}},
		{name: "unpaired source", query: APIEventHandlingQuery{SourceID: proto.String("rack-17")}, bad: true},
		{name: "unpaired source type", query: APIEventHandlingQuery{SourceType: proto.String("Component")}, bad: true},
		{name: "empty source", query: APIEventHandlingQuery{SourceType: proto.String("Rack"), SourceID: proto.String("")}, bad: true},
		{name: "unknown source type", query: APIEventHandlingQuery{SourceType: proto.String("Other"), SourceID: proto.String("source-17")}, bad: true},
		{name: "empty policy filter", query: APIEventHandlingQuery{PolicyID: proto.String("")}, bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			q := tc.query
			q.SiteID = uuid.NewString()
			err := q.Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventHandlingTaskQuery_Validate(t *testing.T) {
	for _, tc := range []struct {
		name  string
		query APIEventHandlingTaskQuery
		bad   bool
	}{
		{name: "all Tasks"},
		{name: "empty event type", query: APIEventHandlingTaskQuery{EventType: proto.String("")}, bad: true},
		{name: "invalid policy UUID", query: APIEventHandlingTaskQuery{PolicyID: proto.String("not-a-uuid")}, bad: true},
		{name: "empty Rack", query: APIEventHandlingTaskQuery{RackID: proto.String("")}, bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			q := tc.query
			q.SiteID = uuid.NewString()
			err := q.Validate()
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIEventHandlingQuery_ToProto(t *testing.T) {
	page := eventPageForTest(t)
	for _, tc := range []struct {
		name       string
		sourceType *string
		wantType   flowv1.EventResourceType
		page       *pagination.PageRequest
		wantPage   *flowv1.Pagination
	}{
		{name: "no exact source"},
		{name: "paginated component source", sourceType: proto.String("Component"), wantType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, page: page, wantPage: &flowv1.Pagination{Offset: 20, Limit: 10}},
		{name: "Rack source", sourceType: proto.String("Rack"), wantType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK},
	} {
		t.Run(tc.name, func(t *testing.T) {
			query := APIEventHandlingQuery{SiteID: uuid.NewString(), EventType: proto.String(EventTypeHardwareLeakDetected), PolicyID: proto.String(uuid.NewString()), RackID: proto.String("rack-17"), SourceType: tc.sourceType}
			if tc.sourceType != nil {
				query.SourceID = proto.String("source-17")
			}
			require.NoError(t, query.Validate())
			want := &flowv1.ListEventHandlingsRequest{Filter: &flowv1.EventHandlingFilter{EventType: query.EventType, PolicyId: &flowv1.UUID{Id: *query.PolicyID}, RackId: query.RackID, ResourceType: tc.wantType, ResourceId: query.SourceID}, Pagination: tc.wantPage}
			require.True(t, proto.Equal(want, query.ToProto(tc.page)))
		})
	}
}

func TestAPIEventHandlingTaskQuery_ToProto(t *testing.T) {
	page := eventPageForTest(t)
	for _, tc := range []struct {
		name  string
		query APIEventHandlingTaskQuery
		page  *pagination.PageRequest
		want  *flowv1.ListEventTasksRequest
	}{
		{name: "no filters", want: &flowv1.ListEventTasksRequest{Filter: &flowv1.EventHandlingFilter{}}},
		{name: "active Tasks without pagination", query: APIEventHandlingTaskQuery{ActiveOnly: true}, want: &flowv1.ListEventTasksRequest{Filter: &flowv1.EventHandlingFilter{}, ActiveOnly: true}},
		{
			name:  "captured associations",
			query: APIEventHandlingTaskQuery{EventType: proto.String(EventTypeHardwareLeakDetected), PolicyID: proto.String("550e8400-e29b-41d4-a716-446655440000"), RackID: proto.String("rack-17"), ActiveOnly: true},
			page:  page,
			want:  &flowv1.ListEventTasksRequest{Filter: &flowv1.EventHandlingFilter{EventType: proto.String(EventTypeHardwareLeakDetected), PolicyId: &flowv1.UUID{Id: "550e8400-e29b-41d4-a716-446655440000"}, RackId: proto.String("rack-17")}, Pagination: &flowv1.Pagination{Offset: 20, Limit: 10}, ActiveOnly: true},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			query := tc.query
			query.SiteID = uuid.NewString()
			require.NoError(t, query.Validate())
			require.True(t, proto.Equal(tc.want, query.ToProto(tc.page)))
		})
	}
}

func eventPageForTest(t *testing.T) *pagination.PageRequest {
	t.Helper()
	number, size := 3, 10
	page := &pagination.PageRequest{PageNumber: &number, PageSize: &size}
	require.NoError(t, page.Validate(nil))
	return page
}

func TestAPIEventHandling_FromProto(t *testing.T) {
	now := time.Now().UTC()
	for _, tc := range []struct {
		name    string
		status  flowv1.EventHandlingStatus
		want    APIEventHandlingStatus
		missing bool
	}{
		{name: "running", status: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RUNNING, want: APIEventHandlingStatusRunning},
		{name: "failed without Tasks", status: flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED, want: APIEventHandlingStatusFailed},
		{name: "unknown", status: flowv1.EventHandlingStatus(1234), want: APIEventHandlingStatusUnknown},
		{name: "missing captured policy", missing: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			wire := &flowv1.EventHandling{EventType: EventTypeHardwareLeakDetected, ResourceId: "source", ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, HandlingStatus: tc.status, AppliedPolicy: &flowv1.EventRulePolicy{}, HandledAt: timePtrToProto(&now)}
			if tc.missing {
				wire.AppliedPolicy = nil
			}
			var got APIEventHandling
			err := got.FromProto(wire)
			if tc.missing {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.want, got.HandlingStatus)
			require.Equal(t, EventTypeHardwareLeakDetected, got.Event.EventType)
			require.NotNil(t, got.Tasks)
			require.Empty(t, got.Tasks)
			require.Nil(t, got.Event.DetectedAt)
			require.NotNil(t, got.AppliedPolicy.Actions)
		})
	}
}

func TestAPIEventAction_OpenAPISchema(t *testing.T) {
	var document map[string]any
	require.NoError(t, yaml.Unmarshal(openapi.Spec, &document))
	compiler := jsonschema.NewCompiler()
	compiler.DefaultDraft(jsonschema.Draft2020)
	compiler.AssertFormat()
	require.NoError(t, compiler.AddResource("event-schema.json", document))
	schema, err := compiler.Compile("event-schema.json#/components/schemas/EventAction")
	require.NoError(t, err)
	for _, tc := range []struct {
		name string
		raw  string
		bad  bool
	}{
		{name: "minimal no-op", raw: `{"name":"record","noAction":{}}`},
		{name: "nullable response fields", raw: `{"name":"record","condition":null,"noAction":{},"submitTask":null,"sendAlert":null}`},
		{name: "conditional Task", raw: `{"name":"shutdown","condition":{"severities":["Critical"]},"submitTask":{"targetStrategy":"Source","onConflict":"Queue","operation":{"powerControl":{"operation":"ForcePowerOff"},"firmwareControl":null}},"noAction":null,"sendAlert":null}`},
		{name: "missing action", raw: `{"name":"record"}`, bad: true},
		{name: "all specifications null", raw: `{"name":"record","noAction":null,"sendAlert":null,"submitTask":null}`, bad: true},
		{name: "multiple actions", raw: `{"name":"record","noAction":{},"sendAlert":{"severity":"Info"}}`, bad: true},
		{name: "empty condition", raw: `{"name":"record","noAction":{},"condition":{"severities":[]}}`, bad: true},
		{name: "two Task operations", raw: `{"name":"do_work","submitTask":{"targetStrategy":"Rack","onConflict":"Reject","operation":{"powerControl":{"operation":"PowerOn"},"firmwareControl":{"operation":"Upgrade"}}}}`, bad: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var value any
			require.NoError(t, json.Unmarshal([]byte(tc.raw), &value))
			err := schema.Validate(value)
			if tc.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}
