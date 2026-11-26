// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/google/uuid"
	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"
	"gopkg.in/yaml.v3"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestAPITaskOperation_Validate(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*APITaskOperation)
		bad    bool
	}{
		{name: "default rule"},
		{name: "pinned rule", change: func(o *APITaskOperation) { o.RuleID = proto.String(uuid.NewString()) }},
		{name: "empty rule", change: func(o *APITaskOperation) { o.RuleID = proto.String("") }, bad: true},
		{name: "malformed rule", change: func(o *APITaskOperation) { o.RuleID = proto.String("invalid") }, bad: true},
		{name: "zero rule", change: func(o *APITaskOperation) { o.RuleID = proto.String(uuid.Nil.String()) }, bad: true},
		{name: "missing operation", change: func(o *APITaskOperation) { o.PowerControl = nil }, bad: true},
		{name: "missing firmware operation", change: func(o *APITaskOperation) {
			o.PowerControl = nil
			o.FirmwareControl = &APIFirmwareControl{}
		}, bad: true},
		{name: "two operations", change: func(o *APITaskOperation) {
			o.FirmwareControl = &APIFirmwareControl{Operation: FirmwareTaskOperationUpgrade}
		}, bad: true},
		{name: "unknown power operation", change: func(o *APITaskOperation) { o.PowerControl.Operation = PowerTaskOperationUnknown }, bad: true},
		{name: "unknown firmware operation", change: func(o *APITaskOperation) {
			o.PowerControl = nil
			o.FirmwareControl = &APIFirmwareControl{Operation: "Other"}
		}, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			operation := APITaskOperation{PowerControl: &APIPowerControl{Operation: PowerTaskOperationPowerOn}}
			if test.change != nil {
				test.change(&operation)
			}
			err := operation.Validate()
			if test.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIPowerControl_Validate(t *testing.T) {
	for _, test := range []struct {
		name      string
		operation APIPowerTaskOperation
		wantErr   string
	}{
		{name: "valid operation", operation: PowerTaskOperationPowerOn},
		{name: "missing operation", wantErr: "cannot be blank"},
		{name: "unsupported operation", operation: PowerTaskOperationUnknown, wantErr: "unsupported power operation"},
	} {
		t.Run(test.name, func(t *testing.T) {
			control := APIPowerControl{Operation: test.operation}
			err := control.Validate()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPIFirmwareControl_Validate(t *testing.T) {
	start := time.Date(2026, 11, 24, 12, 0, 0, 100_000_000, time.UTC)
	end := start.Add(time.Hour)
	sameSecond := start.Add(time.Nanosecond)
	epochSecond := time.Unix(0, 500_000_000).UTC()
	for _, test := range []struct {
		name    string
		start   *time.Time
		end     *time.Time
		wantErr string
	}{
		{name: "no execution window"},
		{name: "start only", start: &start},
		{name: "end only", end: &end},
		{name: "ordered execution window", start: &start, end: &end},
		{name: "end precedes start", start: &end, end: &start, wantErr: "endTime must follow startTime in whole seconds"},
		{name: "later end in same Unix second", start: &start, end: &sameSecond, wantErr: "endTime must follow startTime in whole seconds"},
		{name: "start in Unix second zero", start: &epochSecond, wantErr: "execution window cannot contain Unix second zero"},
		{name: "end in Unix second zero without start", end: &epochSecond, wantErr: "execution window cannot contain Unix second zero"},
	} {
		t.Run(test.name, func(t *testing.T) {
			operation := APIFirmwareControl{Operation: FirmwareTaskOperationUpgrade, StartTime: test.start, EndTime: test.end}
			err := operation.Validate()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPITaskOperation_ToProto(t *testing.T) {
	ruleID := uuid.NewString()
	start := time.Date(2026, 11, 24, 12, 0, 0, 123_456_789, time.UTC)
	end := start.Add(time.Hour + time.Nanosecond)
	for _, test := range []struct {
		name      string
		operation APITaskOperation
		want      *flowv1.TaskOperation
	}{
		{
			name:      "default power operation",
			operation: APITaskOperation{PowerControl: &APIPowerControl{Operation: PowerTaskOperationPowerOff}},
			want:      &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF}}},
		},
		{
			name:      "pinned power operation",
			operation: APITaskOperation{RuleID: &ruleID, PowerControl: &APIPowerControl{Operation: PowerTaskOperationForcePowerOff, OverrideReadinessCheck: true}},
			want:      &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF, OverrideReadinessCheck: true}}},
		},
		{
			name: "pinned firmware upgrade with execution options",
			operation: APITaskOperation{RuleID: &ruleID, FirmwareControl: &APIFirmwareControl{
				Operation: FirmwareTaskOperationUpgrade, TargetVersion: proto.String("2.0.0"),
				StartTime: &start, EndTime: &end, SubTargets: []string{"bmc", "bios"},
				OverrideReadinessCheck: true,
			}},
			want: &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{
				Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE, TargetVersion: proto.String("2.0.0"),
				StartTime: timestamppb.New(start), EndTime: timestamppb.New(end), SubTargets: []string{"bmc", "bios"},
				OverrideReadinessCheck: true,
			}}},
		},
		{
			name:      "firmware defaults preserve absent options",
			operation: APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: FirmwareTaskOperationDowngrade}},
			want:      &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE}}},
		},
		{
			name:      "pinned firmware operation",
			operation: APITaskOperation{RuleID: &ruleID, FirmwareControl: &APIFirmwareControl{Operation: FirmwareTaskOperationRollback, TargetVersion: proto.String("1.2"), OverrideVersionCheck: true}},
			want:      &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK, TargetVersion: proto.String("1.2"), OverrideVersionCheck: true}}},
		},
		{
			name:      "explicit empty firmware version preserves presence",
			operation: APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: FirmwareTaskOperationUpgrade, TargetVersion: proto.String("")}},
			want:      &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE, TargetVersion: proto.String("")}}},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			require.NoError(t, test.operation.Validate())
			got := test.operation.ToProto()
			require.True(t, proto.Equal(test.want, got), "want: %v\ngot: %v", test.want, got)
		})
	}
}

func TestAPITaskOperation_FromProto(t *testing.T) {
	ruleID := uuid.NewString()
	start := time.Date(2026, 11, 24, 13, 15, 0, 987_654_321, time.UTC)
	end := start.Add(2 * time.Hour)
	for _, test := range []struct {
		name  string
		input *flowv1.TaskOperation
		want  APITaskOperation
		bad   bool
	}{
		{
			name:  "pinned power",
			input: &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART, OverrideReadinessCheck: true}}},
			want:  APITaskOperation{RuleID: &ruleID, PowerControl: &APIPowerControl{Operation: "Restart", OverrideReadinessCheck: true}},
		},
		{
			name: "pinned firmware upgrade with execution options",
			input: &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{
				Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE, TargetVersion: proto.String("3.1.4"),
				StartTime: timestamppb.New(start), EndTime: timestamppb.New(end), SubTargets: []string{"bios", "bmc"},
				OverrideReadinessCheck: true,
			}}},
			want: APITaskOperation{RuleID: &ruleID, FirmwareControl: &APIFirmwareControl{
				Operation: "Upgrade", TargetVersion: proto.String("3.1.4"), StartTime: &start, EndTime: &end,
				SubTargets: []string{"bios", "bmc"}, OverrideReadinessCheck: true,
			}},
		},
		{
			name: "firmware rollback with explicit empty version and override",
			input: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{
				Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK, TargetVersion: proto.String(""), OverrideVersionCheck: true,
			}}},
			want: APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: "Rollback", TargetVersion: proto.String(""), OverrideVersionCheck: true}},
		},
		{
			name:  "default clears previous rule and operation",
			input: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE}}},
			want:  APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: "Downgrade"}},
		},
		{
			name:  "unknown power operation remains readable",
			input: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation(999)}}},
			want:  APITaskOperation{PowerControl: &APIPowerControl{Operation: "Unknown"}},
		},
		{
			name:  "unknown firmware operation remains readable",
			input: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation(999)}}},
			want:  APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: "Unknown"}},
		},
		{name: "missing operation", bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			got := APITaskOperation{RuleID: &ruleID, PowerControl: &APIPowerControl{Operation: PowerTaskOperationPowerOn}}
			err := got.FromProto(test.input)
			if test.bad {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, got)
		})
	}
}

func TestAPITaskOperation_JSON(t *testing.T) {
	start := time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)
	end := start.Add(time.Hour)
	for _, test := range []struct {
		name      string
		operation APITaskOperation
		want      string
	}{
		{
			name:      "power control retains default readiness flag",
			operation: APITaskOperation{PowerControl: &APIPowerControl{Operation: PowerTaskOperationPowerOn}},
			want:      `{"ruleId":null,"powerControl":{"operation":"PowerOn","overrideReadinessCheck":false},"firmwareControl":null}`,
		},
		{
			name: "firmware upgrade fields",
			operation: APITaskOperation{FirmwareControl: &APIFirmwareControl{
				Operation: FirmwareTaskOperationUpgrade, TargetVersion: proto.String("2.0.0"),
				StartTime: &start, EndTime: &end, SubTargets: []string{"bmc", "bios"}, OverrideReadinessCheck: true,
			}},
			want: `{"ruleId":null,"powerControl":null,"firmwareControl":{"operation":"Upgrade","targetVersion":"2.0.0","startTime":"2026-11-24T12:00:00Z","endTime":"2026-11-24T13:00:00Z","subTargets":["bmc","bios"],"overrideReadinessCheck":true,"overrideVersionCheck":false}}`,
		},
		{
			name: "firmware rollback retains empty and null options",
			operation: APITaskOperation{FirmwareControl: &APIFirmwareControl{
				Operation: FirmwareTaskOperationRollback, TargetVersion: proto.String(""), SubTargets: []string{}, OverrideVersionCheck: true,
			}},
			want: `{"ruleId":null,"powerControl":null,"firmwareControl":{"operation":"Rollback","targetVersion":"","startTime":null,"endTime":null,"subTargets":[],"overrideReadinessCheck":false,"overrideVersionCheck":true}}`,
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			raw, err := json.Marshal(test.operation)
			require.NoError(t, err)
			require.JSONEq(t, test.want, string(raw))
			var decoded APITaskOperation
			require.NoError(t, json.Unmarshal([]byte(test.want), &decoded))
			require.Equal(t, test.operation, decoded)
			require.NoError(t, decoded.Validate())
		})
	}
}

func TestAPITaskOperation_OpenAPISchema(t *testing.T) {
	var document map[string]any
	require.NoError(t, yaml.Unmarshal(openapi.Spec, &document))
	compiler := jsonschema.NewCompiler()
	compiler.DefaultDraft(jsonschema.Draft2020)
	compiler.AssertFormat()
	require.NoError(t, compiler.AddResource("operations-schema.json", document))
	schema, err := compiler.Compile("operations-schema.json#/components/schemas/TaskOperation")
	require.NoError(t, err)
	for _, test := range []struct {
		name   string
		ruleID string
		bad    bool
	}{
		{name: "default rule", ruleID: "null"},
		{name: "pinned rule", ruleID: `"63f1c140-81c9-4711-8c3a-1664aaed2cd9"`},
		{name: "invalid rule", ruleID: `"invalid"`, bad: true},
		{name: "zero rule", ruleID: `"00000000-0000-0000-0000-000000000000"`, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			var value any
			require.NoError(t, json.Unmarshal([]byte(`{"ruleId":`+test.ruleID+`,"powerControl":{"operation":"ForcePowerOff"},"firmwareControl":null}`), &value))
			err := schema.Validate(value)
			if test.bad {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}
