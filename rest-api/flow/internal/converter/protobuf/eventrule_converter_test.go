// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestEventRuleCreateFrom(t *testing.T) {
	tests := []struct {
		name    string
		mutate  func(*pb.CreateEventRuleRequest)
		wantErr string
	}{
		{name: "valid typed actions"},
		{
			name: "operation omitted",
			mutate: func(req *pb.CreateEventRuleRequest) {
				req.Actions[0].GetSubmitTask().Operation = nil
			},
			wantErr: "task operation is required",
		},
		{
			name: "unspecified power operation",
			mutate: func(req *pb.CreateEventRuleRequest) {
				req.Actions[0].GetSubmitTask().Operation = powerTaskOperation(
					pb.PowerControlOperation_POWER_CONTROL_OPERATION_UNSPECIFIED,
				)
			},
			wantErr: "unsupported power control operation",
		},
		{
			name: "unspecified target strategy",
			mutate: func(req *pb.CreateEventRuleRequest) {
				req.Actions[0].GetSubmitTask().TargetStrategy =
					pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_UNSPECIFIED
			},
			wantErr: "unknown event rule target strategy",
		},
		{
			name: "action spec omitted",
			mutate: func(req *pb.CreateEventRuleRequest) {
				req.Actions[0].Spec = nil
			},
			wantErr: "action spec is required",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			req := validEventRuleCreateRequest()
			if test.mutate != nil {
				test.mutate(req)
			}

			created, err := EventRuleCreateFrom(req)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
			require.Equal(t, eventrule.Type("hardware.leak.detected"), created.EventType)
			require.Len(t, created.Policy.Actions, 3)
			require.IsType(t, &eventrule.SubmitTask{}, created.Policy.Actions[0].Spec)
			require.IsType(t, &eventrule.SendAlert{}, created.Policy.Actions[1].Spec)
			require.IsType(t, &eventrule.Noop{}, created.Policy.Actions[2].Spec)
		})
	}
}

func TestEventRuleTo(t *testing.T) {
	input, err := EventRuleCreateFrom(validEventRuleCreateRequest())
	require.NoError(t, err)
	now := time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC)
	rule := &eventrule.Rule{
		ID:          uuid.New(),
		Origin:      eventrule.RuleOriginPersisted,
		Name:        input.Metadata.Name,
		Description: input.Metadata.Description,
		Enabled:     true,
		EventType:   input.EventType,
		Policy:      input.Policy,
		CreatedAt:   now,
		UpdatedAt:   now,
	}

	tests := []struct {
		name         string
		origin       eventrule.RuleOrigin
		wantReadOnly bool
	}{
		{name: "persisted", origin: eventrule.RuleOriginPersisted},
		{name: "built in", origin: eventrule.RuleOriginBuiltIn, wantReadOnly: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			inputRule := rule.Clone()
			inputRule.Origin = test.origin
			if test.wantReadOnly {
				inputRule.CreatedAt, inputRule.UpdatedAt = time.Time{}, time.Time{}
			}
			converted, err := EventRuleTo(&inputRule)
			require.NoError(t, err)
			require.Equal(t, rule.ID.String(), converted.GetId().GetId())
			require.Equal(t, test.wantReadOnly, converted.GetReadOnly())
			require.Len(t, converted.GetActions(), 3)
			submitTask := converted.GetActions()[0].GetSubmitTask()
			require.NotNil(t, submitTask)
			require.Equal(t, pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF, submitTask.GetOperation().GetPowerControl().GetOperation())
			require.NotNil(t, converted.GetActions()[1].GetSendAlert())
			require.NotNil(t, converted.GetActions()[2].GetNoop())
			if test.wantReadOnly {
				require.Nil(t, converted.GetCreatedAt())
				require.Nil(t, converted.GetUpdatedAt())
			} else {
				require.Equal(t, now, converted.GetCreatedAt().AsTime())
				require.Equal(t, now, converted.GetUpdatedAt().AsTime())
			}
		})
	}

}

func TestEventHandlingTo(t *testing.T) {
	eventID, taskID := uuid.New(), uuid.New()
	handledAt := time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC)
	tests := []struct {
		name            string
		kind            eventrule.ResourceKind
		wantKind        pb.EventResourceType
		policy          eventrule.Policy
		executions      []eventrule.HandlingExecution
		firstObservedAt time.Time
		wantStatus      pb.EventHandlingStatus
		wantReason      pb.EventHandlingReason
		wantMessage     string
		wantError       bool
	}{
		{name: "component snapshot", wantStatus: pb.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING, wantReason: pb.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_FAILED, wantMessage: "A handling action failed; another attempt is scheduled.", firstObservedAt: handledAt.Add(-time.Minute), executions: []eventrule.HandlingExecution{{ID: eventID, ActionName: "record", Status: eventrule.ExecutionStatusDeferred, Reason: eventrule.ExecutionReasonAttemptFailed}}, kind: eventrule.ResourceKindComponent, wantKind: pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, policy: eventrule.Policy{Actions: []eventrule.Action{{Name: "record", Spec: &eventrule.Noop{Reason: "record leakage"}}}}},
		{name: "rack empty snapshot", wantStatus: pb.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION, wantMessage: "No action was required by the applied policy.", executions: []eventrule.HandlingExecution{{ID: eventID, ActionName: "failed", Status: eventrule.ExecutionStatusFailed}}, kind: eventrule.ResourceKindRack, wantKind: pb.EventResourceType_EVENT_RESOURCE_TYPE_RACK},
		{name: "unknown resource", wantStatus: pb.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION, wantMessage: "No action was required by the applied policy.", kind: "unknown", wantKind: pb.EventResourceType_EVENT_RESOURCE_TYPE_UNSPECIFIED},
		{name: "invalid policy", policy: eventrule.Policy{Actions: []eventrule.Action{{Name: "invalid", Spec: &eventrule.SubmitTask{}}}}, wantError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			item := eventrule.Handling{
				ID:              eventID,
				EventType:       eventrule.Type("hardware.temperature.high"),
				EventSummary:    "Temperature exceeded threshold",
				FirstObservedAt: test.firstObservedAt,
				AppliedPolicy:   test.policy,
				Executions:      test.executions,
				ResourceID:      "source-17",
				ResourceType:    test.kind,
				Tasks:           []eventrule.HandlingTask{{ID: taskID, Status: taskcommon.TaskStatusCompleted, CreatedAt: handledAt}},
				HandledAt:       handledAt,
			}
			converted, err := EventHandlingTo(item)
			if test.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, eventID.String(), converted.GetId().GetId())
			require.Equal(t, string(item.EventType), converted.GetEventType())
			require.Equal(t, item.EventSummary, converted.GetEventSummary())
			require.Equal(t, test.wantStatus, converted.GetHandlingStatus())
			require.Equal(t, test.wantReason, converted.GetReason())
			require.Equal(t, test.wantMessage, converted.GetMessage())

			if test.firstObservedAt.IsZero() {
				require.Nil(t, converted.GetEventFirstObservedAt())
			} else {
				require.Equal(t, test.firstObservedAt, converted.GetEventFirstObservedAt().AsTime())
			}
			require.NotNil(t, converted.GetAppliedPolicy())
			actions, err := EventRuleActionsFrom(converted.GetAppliedPolicy().GetActions())
			require.NoError(t, err)
			if len(test.policy.Actions) == 0 {
				require.Empty(t, actions)
			} else {
				require.Equal(t, test.policy.Actions, actions)
			}
			require.Equal(t, item.ResourceID, converted.GetResourceId())
			require.Equal(t, test.wantKind, converted.GetResourceType())
			require.Equal(t, taskID.String(), converted.GetTasks()[0].GetId().GetId())
			require.Equal(t, pb.TaskStatus_TASK_STATUS_COMPLETED, converted.GetTasks()[0].GetStatus())
			require.Equal(t, handledAt, converted.GetTasks()[0].GetCreatedAt().AsTime())
			require.Equal(t, handledAt, converted.GetHandledAt().AsTime())
		})
	}
}

func TestEventRuleListRequestFrom(t *testing.T) {
	enabled := true
	tests := map[string]struct {
		request    *pb.ListEventRulesRequest
		wantOffset int
		wantLimit  int
		wantFilter bool
		wantErr    string
	}{
		"uses default pagination": {
			wantLimit: 100,
		},
		"converts filters and pagination": {
			request: &pb.ListEventRulesRequest{
				EventType: func() *string {
					value := "hardware.leak.detected"
					return &value
				}(),
				Enabled:    &enabled,
				Pagination: &pb.Pagination{Offset: 5, Limit: 10},
			},
			wantOffset: 5,
			wantLimit:  10,
			wantFilter: true,
		},
		"rejects invalid pagination": {
			request: &pb.ListEventRulesRequest{Pagination: &pb.Pagination{}},
			wantErr: "limit must be positive",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			converted, err := EventRuleListRequestFrom(test.request)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
			require.Equal(t, test.wantOffset, converted.Offset)
			require.Equal(t, test.wantLimit, converted.Limit)
			if test.wantFilter {
				require.Equal(
					t,
					eventrule.Type("hardware.leak.detected"),
					*converted.Filter.EventType,
				)
				require.True(t, *converted.Filter.Enabled)
			}
		})
	}
}

func TestTaskOperationFrom(t *testing.T) {
	start := time.Date(2026, 9, 1, 12, 0, 0, 0, time.UTC)
	end := start.Add(time.Hour)
	tests := map[string]struct {
		operation *pb.TaskOperation
		check     func(*testing.T, operations.Operation)
		wantErr   string
	}{
		"power control": {
			operation: powerTaskOperation(
				pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF,
			),
			check: func(t *testing.T, operation operations.Operation) {
				power, ok := operation.(*operations.PowerControlTaskInfo)
				require.True(t, ok)
				require.Equal(t, operations.PowerOperationForcePowerOff, power.Operation)
				require.True(t, power.Forced)
			},
		},
		"firmware control": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						TargetVersion: func() *string {
							value := "2.0.0"
							return &value
						}(),
						StartTime:              timestamppb.New(start),
						EndTime:                timestamppb.New(end),
						SubTargets:             []string{"bmc"},
						OverrideReadinessCheck: true,
						OverrideVersionCheck:   true,
					},
				},
			},
			check: func(t *testing.T, operation operations.Operation) {
				firmware, ok := operation.(*operations.FirmwareControlTaskInfo)
				require.True(t, ok)
				require.Equal(t, operations.FirmwareOperationUpgrade, firmware.Operation)
				require.Equal(t, "2.0.0", firmware.TargetVersion)
				require.Equal(t, start.Unix(), firmware.StartTime)
				require.Equal(t, end.Unix(), firmware.EndTime)
				require.Equal(t, []string{"bmc"}, firmware.SubTargets)
				require.True(t, firmware.OverrideReadinessCheck)
				require.True(t, firmware.OverrideVersionCheck)
			},
		},
		"firmware control without window": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
					},
				},
			},
			check: func(t *testing.T, operation operations.Operation) {
				firmware, ok := operation.(*operations.FirmwareControlTaskInfo)
				require.True(t, ok)
				require.Zero(t, firmware.StartTime)
				require.Zero(t, firmware.EndTime)
			},
		},
		"missing": {
			wantErr: "task operation is required",
		},
		"firmware invalid start timestamp": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						StartTime: &timestamppb.Timestamp{Seconds: 253402300800},
					},
				},
			},
			wantErr: "firmware control start time",
		},
		"firmware epoch end converts to absence sentinel": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						StartTime: timestamppb.New(start),
						EndTime:   timestamppb.New(time.Unix(0, 0)),
					},
				},
			},
			wantErr: "end time: timestamp cannot convert to Unix second zero",
		},
		"firmware converted end precedes start": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						StartTime: timestamppb.New(start.Add(1900 * time.Millisecond)),
						EndTime:   timestamppb.New(start.Add(100 * time.Millisecond)),
					},
				},
			},
			wantErr: "start time must be before end time",
		},
		"firmware subsecond window collapses to one second": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						StartTime: timestamppb.New(start.Add(100 * time.Millisecond)),
						EndTime:   timestamppb.New(start.Add(900 * time.Millisecond)),
					},
				},
			},
			wantErr: "start time must be before end time",
		},
		"firmware fractional timestamps spanning seconds": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						StartTime: timestamppb.New(start.Add(100 * time.Millisecond)),
						EndTime:   timestamppb.New(start.Add(1900 * time.Millisecond)),
					},
				},
			},
			check: func(t *testing.T, operation operations.Operation) {
				firmware, ok := operation.(*operations.FirmwareControlTaskInfo)
				require.True(t, ok)
				require.Equal(t, start.Unix(), firmware.StartTime)
				require.Equal(t, start.Add(time.Second).Unix(), firmware.EndTime)
			},
		},
		"firmware Unix epoch converts to absence sentinel": {
			operation: &pb.TaskOperation{
				Operation: &pb.TaskOperation_FirmwareControl{
					FirmwareControl: &pb.FirmwareControlTaskOperation{
						Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
						StartTime: timestamppb.New(time.Unix(0, 0)),
					},
				},
			},
			wantErr: "start time: timestamp cannot convert to Unix second zero",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			converted, err := TaskOperationFrom(test.operation)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
			test.check(t, converted)
		})
	}
}

func TestTaskOperationTo(t *testing.T) {
	start := time.Date(2026, 9, 1, 12, 0, 0, 0, time.UTC)
	end := start.Add(time.Hour)
	tests := map[string]struct {
		operation operations.Operation
		check     func(*testing.T, *pb.TaskOperation)
		wantErr   string
	}{
		"power control": {
			operation: &operations.PowerControlTaskInfo{
				Operation:              operations.PowerOperationWarmReset,
				OverrideReadinessCheck: true,
			},
			check: func(t *testing.T, converted *pb.TaskOperation) {
				power := converted.GetPowerControl()
				require.Equal(
					t,
					pb.PowerControlOperation_POWER_CONTROL_OPERATION_WARM_RESET,
					power.GetOperation(),
				)
				require.True(t, power.GetOverrideReadinessCheck())
			},
		},
		"firmware control": {
			operation: &operations.FirmwareControlTaskInfo{
				Operation:              operations.FirmwareOperationRollback,
				TargetVersion:          "1.0.0",
				StartTime:              start.Unix(),
				EndTime:                end.Unix(),
				SubTargets:             []string{"bios"},
				OverrideReadinessCheck: true,
				OverrideVersionCheck:   true,
			},
			check: func(t *testing.T, converted *pb.TaskOperation) {
				firmware := converted.GetFirmwareControl()
				require.Equal(
					t,
					pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK,
					firmware.GetOperation(),
				)
				require.Equal(t, "1.0.0", firmware.GetTargetVersion())
				require.Equal(t, start, firmware.GetStartTime().AsTime())
				require.Equal(t, end, firmware.GetEndTime().AsTime())
				require.Equal(t, []string{"bios"}, firmware.GetSubTargets())
				require.True(t, firmware.GetOverrideReadinessCheck())
				require.True(t, firmware.GetOverrideVersionCheck())
			},
		},
		"firmware control equal window": {
			operation: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade,
				StartTime: start.Unix(),
				EndTime:   start.Unix(),
			},
			wantErr: "start time must be before end time",
		},
		"unsupported": {
			operation: &operations.BringUpTaskInfo{},
			wantErr:   "unsupported task operation",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			converted, err := TaskOperationTo(test.operation)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
			test.check(t, converted)
		})
	}
}

func TestEventRuleScopeFrom(t *testing.T) {
	rackID := uuid.New()
	tests := []struct {
		name    string
		scope   *pb.EventRuleScope
		want    eventrule.Scope
		wantErr string
	}{
		{
			name:  "site",
			scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE},
			want:  eventrule.Scope{Type: eventrule.ScopeTypeSite},
		},
		{
			name: "rack",
			scope: &pb.EventRuleScope{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				Id:   UUIDTo(rackID),
			},
			want: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID},
		},
		{
			name: "site with ID",
			scope: &pb.EventRuleScope{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
				Id:   UUIDTo(rackID),
			},
			wantErr: "site scope must not have an id",
		},
		{
			name: "malformed rack ID",
			scope: &pb.EventRuleScope{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				Id:   &pb.UUID{Id: "invalid"},
			},
			wantErr: "valid non-zero UUID",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := EventRuleScopeFrom(test.scope)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
			require.Equal(t, test.want, got)
		})
	}
}

func validEventRuleCreateRequest() *pb.CreateEventRuleRequest {
	return &pb.CreateEventRuleRequest{
		Name:        "leak response",
		Description: "typed event rule",
		EventType:   "hardware.leak.detected",
		Actions: []*pb.EventRuleAction{
			{
				Name: "power_off",
				Condition: &pb.EventRuleActionCondition{
					Severities:     []pb.EventRuleSeverity{pb.EventRuleSeverity_EVENT_RULE_SEVERITY_CRITICAL},
					ComponentTypes: []pb.ComponentType{pb.ComponentType_COMPONENT_TYPE_COMPUTE},
				},
				Spec: &pb.EventRuleAction_SubmitTask{
					SubmitTask: &pb.EventRuleSubmitTaskAction{
						Operation: powerTaskOperation(
							pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF,
						),
						TargetStrategy:   pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS,
						ConflictStrategy: pb.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE,
						Description:      "power off affected components",
					},
				},
			},
			{
				Name: "alert",
				Spec: &pb.EventRuleAction_SendAlert{
					SendAlert: &pb.EventRuleSendAlertAction{
						Severity: pb.EventRuleSeverity_EVENT_RULE_SEVERITY_WARNING,
						Message:  "leak detected",
					},
				},
			},
			{
				Name: "audit",
				Spec: &pb.EventRuleAction_Noop{
					Noop: &pb.EventRuleNoopAction{Reason: "record only"},
				},
			},
		},
	}
}

func powerTaskOperation(operation pb.PowerControlOperation) *pb.TaskOperation {
	return &pb.TaskOperation{
		Operation: &pb.TaskOperation_PowerControl{
			PowerControl: &pb.PowerControlTaskOperation{Operation: operation},
		},
	}
}

func TestEventRuleTargetStrategyFrom(t *testing.T) {
	tests := []struct {
		name       string
		wire       []byte
		want       eventrule.TargetStrategy
		wantNumber int32
	}{
		{name: "source retains the legacy component wire number", wire: []byte{0x20, 0x01}, want: eventrule.TargetStrategySource, wantNumber: 1},
		{name: "legacy rack retains field and enum numbers", wire: []byte{0x20, 0x02}, want: eventrule.TargetStrategyRack, wantNumber: 2},
		{name: "legacy affected retains field and enum numbers", wire: []byte{0x20, 0x03}, want: eventrule.TargetStrategyAffectedComponents, wantNumber: 3},
		{name: "unknown future strategy", wire: []byte{0x20, 0x63}, wantNumber: 99},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var action pb.EventRuleSubmitTaskAction
			require.NoError(t, proto.Unmarshal(test.wire, &action))
			require.Equal(t, test.wantNumber, int32(action.GetTargetStrategy()))
			strategy, err := eventRuleTargetStrategyFrom(action.GetTargetStrategy())
			if test.want == "" {
				require.ErrorContains(t, err, "unknown event rule target strategy")
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, strategy)
			encoded, err := proto.Marshal(&pb.EventRuleSubmitTaskAction{TargetStrategy: eventRuleTargetStrategyTo(strategy)})
			require.NoError(t, err)
			require.Equal(t, test.wire, encoded)
		})
	}
	for _, name := range []string{"EVENT_RULE_TARGET_STRATEGY_COMPONENT", "EVENT_RULE_TARGET_STRATEGY_SOURCE"} {
		t.Run(name, func(t *testing.T) {
			var action pb.EventRuleSubmitTaskAction
			require.NoError(t, protojson.Unmarshal([]byte(`{"targetStrategy":"`+name+`"}`), &action))
			strategy, err := eventRuleTargetStrategyFrom(action.GetTargetStrategy())
			require.NoError(t, err)
			require.Equal(t, eventrule.TargetStrategySource, strategy)
			encoded, err := protojson.Marshal(&pb.EventRuleSubmitTaskAction{TargetStrategy: eventRuleTargetStrategyTo(strategy)})
			require.NoError(t, err)
			require.JSONEq(t, `{"targetStrategy":"EVENT_RULE_TARGET_STRATEGY_COMPONENT"}`, string(encoded))
		})
	}

}
