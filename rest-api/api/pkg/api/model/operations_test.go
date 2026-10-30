// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/google/uuid"
	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
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
		{name: "two operations", change: func(o *APITaskOperation) { o.FirmwareControl = &APIFirmwareControl{Operation: "Upgrade"} }, bad: true},
		{name: "unknown power operation", change: func(o *APITaskOperation) { o.PowerControl.Operation = "Unknown" }, bad: true},
		{name: "unknown firmware operation", change: func(o *APITaskOperation) {
			o.PowerControl = nil
			o.FirmwareControl = &APIFirmwareControl{Operation: "Other"}
		}, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			operation := APITaskOperation{PowerControl: &APIPowerControl{Operation: "PowerOn"}}
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

func TestAPITaskOperation_ToProto(t *testing.T) {
	ruleID := uuid.NewString()
	for _, test := range []struct {
		name      string
		operation APITaskOperation
		want      *flowv1.TaskOperation
	}{
		{
			name:      "default power operation",
			operation: APITaskOperation{PowerControl: &APIPowerControl{Operation: "PowerOff"}},
			want:      &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF}}},
		},
		{
			name:      "pinned power operation",
			operation: APITaskOperation{RuleID: &ruleID, PowerControl: &APIPowerControl{Operation: "ForcePowerOff", OverrideReadinessCheck: true}},
			want:      &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF, OverrideReadinessCheck: true}}},
		},
		{
			name:      "pinned firmware operation",
			operation: APITaskOperation{RuleID: &ruleID, FirmwareControl: &APIFirmwareControl{Operation: "Rollback", TargetVersion: proto.String("1.2"), OverrideVersionCheck: true}},
			want:      &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK, TargetVersion: proto.String("1.2"), OverrideVersionCheck: true}}},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			require.NoError(t, test.operation.Validate())
			require.True(t, proto.Equal(test.want, test.operation.ToProto()))
		})
	}
}

func TestAPITaskOperation_FromProto(t *testing.T) {
	ruleID := uuid.NewString()
	for _, test := range []struct {
		name  string
		input *flowv1.TaskOperation
		want  APITaskOperation
		bad   bool
	}{
		{
			name:  "pinned power",
			input: &flowv1.TaskOperation{RuleId: &flowv1.UUID{Id: ruleID}, Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART}}},
			want:  APITaskOperation{RuleID: &ruleID, PowerControl: &APIPowerControl{Operation: "Restart"}},
		},
		{
			name:  "default clears previous rule and operation",
			input: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE}}},
			want:  APITaskOperation{FirmwareControl: &APIFirmwareControl{Operation: "Downgrade"}},
		},
		{name: "missing operation", bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			got := APITaskOperation{RuleID: &ruleID, PowerControl: &APIPowerControl{Operation: "PowerOn"}}
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
