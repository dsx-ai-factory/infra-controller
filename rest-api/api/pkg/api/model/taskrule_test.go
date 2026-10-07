// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func sampleRuleDefinition() APITaskRuleDefinition {
	return APITaskRuleDefinition{
		Version: "v1",
		Steps: []APITaskRuleSequenceStep{
			{
				ComponentType: "Compute",
				Stage:         1,
				MaxParallel:   4,
				Timeout:       "60s",
				PreOperation: []APITaskRuleActionConfig{
					{Name: "VerifyReachability", Timeout: "10s"},
				},
				MainOperation: APITaskRuleActionConfig{
					Name:    "PowerControl",
					Timeout: "30s",
					Parameters: map[string]any{
						"operation": "on",
					},
				},
				Retry: &APITaskRuleRetryPolicy{
					MaxAttempts:        3,
					InitialInterval:    "1s",
					BackoffCoefficient: 2.0,
					MaxInterval:        "30s",
				},
			},
		},
	}
}

func TestAPICreateRuleRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APITaskRuleCreateRequest
		wantErr string
	}{
		{
			name: "valid",
			req: APITaskRuleCreateRequest{
				SiteID:         "site-id",
				Name:           "my-rule",
				OperationType:  APIOperationTypePowerControl,
				OperationCode:  "power_on",
				RuleDefinition: sampleRuleDefinition(),
			},
		},
		{
			name: "missing siteId",
			req: APITaskRuleCreateRequest{
				Name:          "x",
				OperationType: APIOperationTypePowerControl,
				OperationCode: "power_on",
			},
			wantErr: "siteId is required",
		},
		{
			name: "missing name",
			req: APITaskRuleCreateRequest{
				SiteID:        "site-id",
				OperationType: APIOperationTypePowerControl,
				OperationCode: "power_on",
			},
			wantErr: "name is required",
		},
		{
			name: "missing operationType",
			req: APITaskRuleCreateRequest{
				SiteID:        "site-id",
				Name:          "x",
				OperationCode: "power_on",
			},
			wantErr: "operationType is required",
		},
		{
			name: "unknown operationType",
			req: APITaskRuleCreateRequest{
				SiteID:        "site-id",
				Name:          "x",
				OperationType: "bogus",
				OperationCode: "power_on",
			},
			wantErr: "operationType must be one of",
		},
		{
			name: "missing operationCode",
			req: APITaskRuleCreateRequest{
				SiteID:        "site-id",
				Name:          "x",
				OperationType: APIOperationTypePowerControl,
			},
			wantErr: "operationCode is required",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}

func TestAPICreateRuleRequest_ToProto(t *testing.T) {
	req := APITaskRuleCreateRequest{
		SiteID:         "site-id",
		Name:           "my-rule",
		Description:    "desc",
		OperationType:  APIOperationTypeFirmwareControl,
		OperationCode:  "upgrade",
		RuleDefinition: sampleRuleDefinition(),
	}

	pb, err := req.ToProto()
	require.NoError(t, err)
	require.NotNil(t, pb)
	assert.Equal(t, "my-rule", pb.GetName())
	assert.Equal(t, "desc", pb.GetDescription())
	assert.Equal(t, flowv1.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL, pb.GetOperationType())
	assert.Equal(t, "upgrade", pb.GetOperationCode())

	// Round-trip the rule definition JSON to make sure snake_case is preserved.
	var rd map[string]any
	require.NoError(t, json.Unmarshal([]byte(pb.GetRuleDefinitionJson()), &rd))
	assert.Equal(t, "v1", rd["version"])
	steps, ok := rd["steps"].([]any)
	require.True(t, ok)
	require.Len(t, steps, 1)
	step := steps[0].(map[string]any)
	assert.Equal(t, "Compute", step["component_type"])
	assert.Equal(t, float64(4), step["max_parallel"])
	main := step["main_operation"].(map[string]any)
	assert.Equal(t, "PowerControl", main["name"])
}

func TestAPIUpdateRuleRequest_Validate(t *testing.T) {
	name := "new-name"
	tests := []struct {
		name    string
		req     APITaskRuleUpdateRequest
		wantErr string
	}{
		{
			name: "valid - rename only",
			req: APITaskRuleUpdateRequest{
				SiteID: "site-id",
				Name:   &name,
			},
		},
		{
			name:    "missing siteId",
			req:     APITaskRuleUpdateRequest{Name: &name},
			wantErr: "siteId is required",
		},
		{
			name:    "no fields to update",
			req:     APITaskRuleUpdateRequest{SiteID: "site-id"},
			wantErr: "at least one of name",
		},
		{
			name: "empty name explicitly",
			req: APITaskRuleUpdateRequest{
				SiteID: "site-id",
				Name:   stringPtr(""),
			},
			wantErr: "name cannot be empty",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}

func TestAPIUpdateRuleRequest_ToProto(t *testing.T) {
	name := "new-name"
	desc := "new-desc"
	rd := sampleRuleDefinition()
	req := APITaskRuleUpdateRequest{
		SiteID:         "site-id",
		Name:           &name,
		Description:    &desc,
		RuleDefinition: &rd,
	}

	pb, err := req.ToProto("rule-id")
	require.NoError(t, err)
	assert.Equal(t, "rule-id", pb.GetRuleId().GetId())
	assert.Equal(t, "new-name", pb.GetName())
	assert.Equal(t, "new-desc", pb.GetDescription())
	require.NotNil(t, pb.RuleDefinitionJson)
	assert.Contains(t, *pb.RuleDefinitionJson, `"component_type":"Compute"`)
}

func TestAPIUpdateRuleRequest_ToProto_OmitsRuleDef(t *testing.T) {
	name := "new-name"
	req := APITaskRuleUpdateRequest{
		SiteID: "site-id",
		Name:   &name,
	}
	pb, err := req.ToProto("rule-id")
	require.NoError(t, err)
	assert.Nil(t, pb.RuleDefinitionJson)
}

func TestAPIGetRuleRequest_Validate(t *testing.T) {
	require.Error(t, (&APITaskRuleGetRequest{}).Validate())
	require.NoError(t, (&APITaskRuleGetRequest{SiteID: "site-id"}).Validate())
}

func TestAPIDeleteRuleRequest_Validate(t *testing.T) {
	require.Error(t, (&APITaskRuleDeleteRequest{}).Validate())
	require.NoError(t, (&APITaskRuleDeleteRequest{SiteID: "site-id"}).Validate())
}

func TestAPIListRulesRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APITaskRuleGetAllRequest
		wantErr string
	}{
		{name: "valid - no filters", req: APITaskRuleGetAllRequest{SiteID: "site-id"}},
		{
			name: "valid - filters",
			req: APITaskRuleGetAllRequest{
				SiteID:        "site-id",
				OperationType: APIOperationTypePowerControl,
			},
		},
		{name: "missing siteId", req: APITaskRuleGetAllRequest{}, wantErr: "siteId"},
		{
			name:    "unknown operationType",
			req:     APITaskRuleGetAllRequest{SiteID: "site-id", OperationType: "bogus"},
			wantErr: "operationType must be one of",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}

func TestAPIListRulesRequest_ToProto(t *testing.T) {
	pageNum, pageSize := 2, 10
	req := APITaskRuleGetAllRequest{
		SiteID:        "site-id",
		OperationType: APIOperationTypePowerControl,
	}
	page := pagination.PageRequest{PageNumber: &pageNum, PageSize: &pageSize}

	pb, err := req.ToProto(page)
	require.NoError(t, err)
	require.NotNil(t, pb.OperationType)
	assert.Equal(t, flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL, *pb.OperationType)
	require.NotNil(t, pb.Limit)
	assert.Equal(t, int32(10), *pb.Limit)
	require.NotNil(t, pb.Offset)
	assert.Equal(t, int32(10), *pb.Offset) // (page=2 - 1) * size=10
}

func TestAPIListRulesRequest_QueryValues(t *testing.T) {
	pageNum, pageSize := 1, 50
	req := APITaskRuleGetAllRequest{
		SiteID:        "site-id",
		OperationType: APIOperationTypePowerControl,
	}
	page := pagination.PageRequest{PageNumber: &pageNum, PageSize: &pageSize}

	v := req.QueryValues(page)
	assert.Equal(t, "site-id", v.Get("siteId"))
	assert.Equal(t, string(APIOperationTypePowerControl), v.Get("operationType"))
	assert.Equal(t, "1", v.Get("pageNumber"))
	assert.Equal(t, "50", v.Get("pageSize"))
}

func TestAPIOperationRule_FromProto(t *testing.T) {
	created := time.Date(2026, 6, 6, 12, 0, 0, 0, time.UTC)
	updated := created.Add(time.Hour)
	rdJSON, err := sampleRuleDefinition().toFlowJSON()
	require.NoError(t, err)

	pbRule := &flowv1.OperationRule{
		Id:                 &flowv1.UUID{Id: "rule-id"},
		Name:               "my-rule",
		Description:        "desc",
		OperationType:      flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL,
		OperationCode:      "power_on",
		RuleDefinitionJson: rdJSON,
		IsDefault:          true,
		CreatedAt:          timestamppb.New(created),
		UpdatedAt:          timestamppb.New(updated),
	}

	got := &APITaskRule{}
	require.NoError(t, got.FromProto(pbRule))
	assert.Equal(t, "rule-id", got.ID)
	assert.Equal(t, "my-rule", got.Name)
	assert.Equal(t, "desc", got.Description)
	assert.Equal(t, APIOperationTypePowerControl, got.OperationType)
	assert.Equal(t, "power_on", got.OperationCode)
	assert.True(t, got.IsDefault)
	assert.Equal(t, created, got.Created)
	assert.Equal(t, updated, got.Updated)
	assert.Equal(t, "v1", got.RuleDefinition.Version)
	require.Len(t, got.RuleDefinition.Steps, 1)
	assert.Equal(t, "Compute", got.RuleDefinition.Steps[0].ComponentType)
	assert.Equal(t, "PowerControl", got.RuleDefinition.Steps[0].MainOperation.Name)
}

func TestAPIOperationRule_FromProto_InvalidJSON(t *testing.T) {
	pbRule := &flowv1.OperationRule{
		Id:                 &flowv1.UUID{Id: "rule-id"},
		RuleDefinitionJson: "not valid json",
	}
	got := &APITaskRule{}
	err := got.FromProto(pbRule)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "invalid ruleDefinition")
}

func TestAPIOperationRule_FromProto_NilSafe(t *testing.T) {
	r := &APITaskRule{}
	assert.NoError(t, r.FromProto(nil))
}

func stringPtr(s string) *string { return &s }

// TestRequestRuleIDValidation exercises the ruleId field across the rack,
// tray, and domain operation request models. Their validation paths converge
// on the same "nil or valid UUID" contract.
func TestRequestRuleIDValidation(t *testing.T) {
	validUUID := "550e8400-e29b-41d4-a716-446655440000"
	badUUID := "not-a-uuid"

	tests := []struct {
		name        string
		validate    func() error
		wantErr     bool
		errContains string
	}{
		// ---- power.go ----
		{
			name: "APIUpdatePowerStateRequest - bad ruleId",
			validate: func() error {
				return (&APIUpdatePowerStateRequest{SiteID: "s", State: "on", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIUpdatePowerStateRequest - valid ruleId",
			validate: func() error {
				return (&APIUpdatePowerStateRequest{SiteID: "s", State: "on", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchUpdateRackPowerStateRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchUpdateRackPowerStateRequest{SiteID: "s", State: "on", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchUpdateRackPowerStateRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchUpdateRackPowerStateRequest{SiteID: "s", State: "on", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchUpdateTrayPowerStateRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchUpdateTrayPowerStateRequest{SiteID: "s", State: "on", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchUpdateTrayPowerStateRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchUpdateTrayPowerStateRequest{SiteID: "s", State: "on", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchUpdateNVLinkDomainPowerStateRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchUpdateNVLinkDomainPowerStateRequest{
					SiteID:          validUUID,
					NVLinkDomainIDs: []string{validUUID},
					State:           "on",
					RuleID:          &badUUID,
				}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchUpdateNVLinkDomainPowerStateRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchUpdateNVLinkDomainPowerStateRequest{
					SiteID:          validUUID,
					NVLinkDomainIDs: []string{validUUID},
					State:           "on",
					RuleID:          &validUUID,
				}).Validate()
			},
		},
		// ---- firmware.go ----
		{
			name: "APIUpdateFirmwareRequest - bad ruleId",
			validate: func() error {
				return (&APIUpdateFirmwareRequest{SiteID: "s", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIUpdateFirmwareRequest - valid ruleId",
			validate: func() error {
				return (&APIUpdateFirmwareRequest{SiteID: "s", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchRackFirmwareUpdateRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchRackFirmwareUpdateRequest{SiteID: "s", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchRackFirmwareUpdateRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchRackFirmwareUpdateRequest{SiteID: "s", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchTrayFirmwareUpdateRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchTrayFirmwareUpdateRequest{SiteID: "s", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchTrayFirmwareUpdateRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchTrayFirmwareUpdateRequest{SiteID: "s", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APINVLinkDomainFirmwareUpdateRequest - bad ruleId",
			validate: func() error {
				return (&APINVLinkDomainFirmwareUpdateRequest{SiteID: validUUID, RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APINVLinkDomainFirmwareUpdateRequest - valid ruleId",
			validate: func() error {
				return (&APINVLinkDomainFirmwareUpdateRequest{SiteID: validUUID, RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchNVLinkDomainFirmwareUpdateRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchNVLinkDomainFirmwareUpdateRequest{
					SiteID:          validUUID,
					NVLinkDomainIDs: []string{validUUID},
					RuleID:          &badUUID,
				}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchNVLinkDomainFirmwareUpdateRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchNVLinkDomainFirmwareUpdateRequest{
					SiteID:          validUUID,
					NVLinkDomainIDs: []string{validUUID},
					RuleID:          &validUUID,
				}).Validate()
			},
		},
		// ---- rack.go (bring-up) ----
		{
			name: "APIBringUpRackRequest - bad ruleId",
			validate: func() error {
				return (&APIBringUpRackRequest{SiteID: "s", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBringUpRackRequest - valid ruleId",
			validate: func() error {
				return (&APIBringUpRackRequest{SiteID: "s", RuleID: &validUUID}).Validate()
			},
		},
		{
			name: "APIBatchBringUpRackRequest - bad ruleId",
			validate: func() error {
				return (&APIBatchBringUpRackRequest{SiteID: "s", RuleID: &badUUID}).Validate()
			},
			wantErr:     true,
			errContains: "UUID",
		},
		{
			name: "APIBatchBringUpRackRequest - valid ruleId",
			validate: func() error {
				return (&APIBatchBringUpRackRequest{SiteID: "s", RuleID: &validUUID}).Validate()
			},
		},
		// ---- nil vs empty pointer (all types share the same optional-UUID semantics) ----
		{
			name: "APIBringUpRackRequest - nil ruleId is ok",
			validate: func() error {
				return (&APIBringUpRackRequest{SiteID: "s"}).Validate()
			},
		},
		{
			name: "APIBringUpRackRequest - empty-string ruleId is ok (treated as unset)",
			validate: func() error {
				empty := ""
				return (&APIBringUpRackRequest{SiteID: "s", RuleID: &empty}).Validate()
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.validate()
			if tt.wantErr {
				if assert.Error(t, err) && tt.errContains != "" {
					assert.Contains(t, err.Error(), tt.errContains)
				}
				return
			}
			assert.NoError(t, err)
		})
	}
}
