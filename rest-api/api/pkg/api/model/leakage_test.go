// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"
)

func TestAPIPutLeakagePolicyRequest_Validate(t *testing.T) {
	target, affected := LeakageTargetComponent, LeakageTargetAffected
	queue := LeakageConflictQueue
	tests := []struct {
		name      string
		request   APIPutLeakagePolicyRequest
		wantError bool
	}{
		{name: "force power off", request: APIPutLeakagePolicyRequest{Action: LeakageActionForcePowerOff, Target: &affected, OnConflict: &queue}},
		{name: "no action", request: APIPutLeakagePolicyRequest{Action: LeakageActionNoAction}},
		{name: "force missing target", request: APIPutLeakagePolicyRequest{Action: LeakageActionForcePowerOff, OnConflict: &queue}, wantError: true},
		{name: "no action with target", request: APIPutLeakagePolicyRequest{Action: LeakageActionNoAction, Target: &target}, wantError: true},
		{name: "unknown action", request: APIPutLeakagePolicyRequest{Action: "Disabled"}, wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantError {
				require.Error(t, err)
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
		{name: "built in shutdown", response: policyResponse(true, forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE), now), want: APILeakagePolicy{Action: LeakageActionForcePowerOff, Target: stringPointer(LeakageTargetAffected), OnConflict: stringPointer(LeakageConflictQueue)}},
		{name: "component reject", response: policyResponse(false, forcePowerAction(flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_COMPONENT, flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT), now), want: APILeakagePolicy{Action: LeakageActionForcePowerOff, Target: stringPointer(LeakageTargetComponent), OnConflict: stringPointer(LeakageConflictReject), Updated: &now}},
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
		})
	}
}

func TestNewAPILeakageHandling(t *testing.T) {
	handledAt := time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC)
	got := NewAPILeakageHandling(&flowv1.EventHandling{
		Id:           &flowv1.UUID{Id: "handling-id"},
		EventType:    string(flowtypes.EventTypeHardwareLeakDetected),
		ResourceId:   "component-17",
		ResourceType: flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT,
		Tasks: []*flowv1.EventHandlingTask{{
			Id:        &flowv1.UUID{Id: "task-id"},
			Status:    flowv1.TaskStatus_TASK_STATUS_RUNNING,
			CreatedAt: timestamppb.New(handledAt),
		}},
		HandledAt: timestamppb.New(handledAt),
	})
	assert.Equal(t, APILeakageHandling{
		ID:             "handling-id",
		SourceID:       "component-17",
		SourceType:     LeakageHandlingSourceTypeComponent,
		HandlingStatus: APILeakHandlingStatusShuttingDown,
		TaskIDs:        []string{"task-id"},
		HandledAt:      handledAt,
	}, got)
}

func TestLeakageHandlingStatus(t *testing.T) {
	tests := []struct {
		name  string
		tasks []*flowv1.EventHandlingTask
		want  APILeakHandlingStatus
	}{
		{name: "no tasks", want: APILeakHandlingStatusNone},
		{name: "active", tasks: []*flowv1.EventHandlingTask{{Status: flowv1.TaskStatus_TASK_STATUS_PENDING}}, want: APILeakHandlingStatusShuttingDown},
		{name: "completed", tasks: []*flowv1.EventHandlingTask{{Status: flowv1.TaskStatus_TASK_STATUS_COMPLETED}}, want: APILeakHandlingStatusDown},
		{name: "failed", tasks: []*flowv1.EventHandlingTask{{Status: flowv1.TaskStatus_TASK_STATUS_FAILED}}, want: APILeakHandlingStatusFailed},
		{name: "unknown", tasks: []*flowv1.EventHandlingTask{{Status: flowv1.TaskStatus_TASK_STATUS_UNKNOWN}}, want: APILeakHandlingStatusUnknown},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, leakageHandlingStatus(test.tasks))
		})
	}
}

func TestAPILeakageHandlingQuery_Validate(t *testing.T) {
	tests := []struct {
		name      string
		query     APILeakageHandlingQuery
		wantError bool
	}{
		{name: "component default limit", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeComponent, SourceID: "component-17"}},
		{name: "rack page", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeRack, SourceID: "rack-17", Limit: int32Pointer(20), Cursor: "cursor"}},
		{name: "missing site", query: APILeakageHandlingQuery{}, wantError: true},
		{name: "invalid source type", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: "Site", SourceID: "site-17"}, wantError: true},
		{name: "missing source ID", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeComponent}, wantError: true},
		{name: "zero limit", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeComponent, SourceID: "component-17", Limit: int32Pointer(0)}, wantError: true},
		{name: "excessive limit", query: APILeakageHandlingQuery{SiteID: "site-id", SourceType: LeakageHandlingSourceTypeComponent, SourceID: "component-17", Limit: int32Pointer(101)}, wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.query.Validate()
			if test.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPILeakageHandlingPage_MarshalJSON(t *testing.T) {
	encoded, err := json.Marshal(APILeakageHandlingPage{Items: []APILeakageHandling{}})
	require.NoError(t, err)
	assert.JSONEq(t, `{"items":[],"nextCursor":null}`, string(encoded))
}

func int32Pointer(value int32) *int32 { return &value }

func policyResponse(readOnly bool, action *flowv1.EventRuleAction, updated time.Time) *flowv1.EventRule {
	return &flowv1.EventRule{ReadOnly: readOnly, Enabled: true, EventType: leakageEventTypeForTest, Actions: []*flowv1.EventRuleAction{action}, UpdatedAt: timestamppb.New(updated)}
}

const leakageEventTypeForTest = string(flowtypes.EventTypeHardwareLeakDetected)

func forcePowerAction(target flowv1.EventRuleTargetStrategy, conflict flowv1.EventRuleConflictStrategy) *flowv1.EventRuleAction {
	return &flowv1.EventRuleAction{Spec: &flowv1.EventRuleAction_SubmitTask{SubmitTask: &flowv1.EventRuleSubmitTaskAction{TargetStrategy: target, ConflictStrategy: conflict, Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF}}}}}}
}
func stringPointer(value string) *string { return &value }
