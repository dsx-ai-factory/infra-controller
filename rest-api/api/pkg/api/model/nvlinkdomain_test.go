// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"strings"
	"testing"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestValidateNVLinkDomainID(t *testing.T) {
	tests := []struct {
		name           string
		nvLinkDomainID string
		wantErr        bool
	}{
		{name: "accepts UUID", nvLinkDomainID: uuid.NewString()},
		{name: "rejects empty ID", wantErr: true},
		{name: "rejects malformed UUID", nvLinkDomainID: "not-a-uuid", wantErr: true},
		{name: "rejects zero UUID", nvLinkDomainID: uuid.Nil.String(), wantErr: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := ValidateNVLinkDomainID(test.nvLinkDomainID)
			if test.wantErr {
				require.EqualError(t, err, "NVLink Domain ID must be a non-zero UUID")
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestNVLinkDomainTargetSpec(t *testing.T) {
	nvLinkDomainIDs := []string{uuid.NewString(), uuid.NewString()}

	got := NVLinkDomainTargetSpec(nvLinkDomainIDs)
	targets := got.GetNvlDomains().GetTargets()

	require.Len(t, targets, 2)
	assert.Equal(t, nvLinkDomainIDs[0], targets[0].GetId().GetId())
	assert.Equal(t, nvLinkDomainIDs[1], targets[1].GetId().GetId())
	assert.Empty(t, targets[0].GetComponentTypes())
	assert.IsType(t, &flowv1.OperationTargetSpec_NvlDomains{}, got.GetTargets())
}

func TestAPIBatchUpdateNVLinkDomainPowerStateRequest_Validate(t *testing.T) {
	nvLinkDomainID := uuid.NewString()
	otherDomainID := uuid.NewString()
	ruleID := uuid.NewString()
	badRuleID := "not-a-uuid"
	errorContains := func(want string) func(*testing.T, error) {
		return func(t *testing.T, err error) {
			t.Helper()
			require.ErrorContains(t, err, want)
		}
	}

	tests := []struct {
		name      string
		request   APIBatchUpdateNVLinkDomainPowerStateRequest
		assertErr func(*testing.T, error)
	}{
		{
			name: "accepts valid request",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID},
				State:           PowerControlStateOn,
				RuleID:          &ruleID,
			},
		},
		{
			name: "accepts multiple unique domain UUIDs",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID, otherDomainID},
				State:           PowerControlStateOn,
			},
		},
		{
			name: "rejects missing site ID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				NVLinkDomainIDs: []string{nvLinkDomainID},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("siteId is required"),
		},
		{
			name: "rejects malformed site ID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          "not-a-uuid",
				NVLinkDomainIDs: []string{nvLinkDomainID},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains(validationErrorInvalidUUID),
		},
		{
			name: "rejects missing domains",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID: uuid.NewString(),
				State:  PowerControlStateOn,
			},
			assertErr: errorContains("domainIds must contain at least one NVLink Domain ID"),
		},
		{
			name: "rejects malformed domain UUID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{"not-a-uuid"},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("0: NVLink Domain ID must be a non-zero UUID"),
		},
		{
			name: "rejects zero domain UUID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{uuid.Nil.String()},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("0: NVLink Domain ID must be a non-zero UUID"),
		},
		{
			name: "rejects semantically duplicate domain UUID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID, strings.ToUpper(nvLinkDomainID)},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("1: duplicates NVLink Domain ID " + strings.ToUpper(nvLinkDomainID)),
		},
		{
			name: "rejects invalid power state",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID},
				State:           "hibernate",
			},
			assertErr: errorContains("must be one of"),
		},
		{
			name: "rejects invalid rule ID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID},
				State:           PowerControlStateOn,
				RuleID:          &badRuleID,
			},
			assertErr: errorContains(validationErrorInvalidUUID),
		},
		{
			name: "returns structured errors for all invalid fields",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				NVLinkDomainIDs: []string{"not-a-uuid"},
			},
			assertErr: func(t *testing.T, err error) {
				t.Helper()
				var validationErrors validation.Errors
				require.ErrorAs(t, err, &validationErrors)
				assert.Contains(t, validationErrors, "siteId")
				assert.Contains(t, validationErrors, "domainIds")
				assert.Contains(t, validationErrors, "state")

				encoded, marshalErr := json.Marshal(err)
				require.NoError(t, marshalErr)
				assert.JSONEq(t, `{
					"domainIds":{"0":"NVLink Domain ID must be a non-zero UUID"},
					"siteId":"siteId is required",
					"state":"a value is required"
				}`, string(encoded))
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.request.Validate()
			if test.assertErr != nil {
				test.assertErr(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPINVLinkDomainFirmwareUpdateRequest_Validate(t *testing.T) {
	badRuleID := "not-a-uuid"
	tests := []struct {
		name    string
		request APINVLinkDomainFirmwareUpdateRequest
		wantErr bool
	}{
		{name: "accepts site only", request: APINVLinkDomainFirmwareUpdateRequest{SiteID: uuid.NewString()}},
		{name: "rejects missing site", request: APINVLinkDomainFirmwareUpdateRequest{}, wantErr: true},
		{name: "rejects malformed site", request: APINVLinkDomainFirmwareUpdateRequest{SiteID: "not-a-uuid"}, wantErr: true},
		{
			name: "rejects invalid rule ID",
			request: APINVLinkDomainFirmwareUpdateRequest{
				SiteID: uuid.NewString(),
				RuleID: &badRuleID,
			},
			wantErr: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPIBatchNVLinkDomainFirmwareUpdateRequest_Validate(t *testing.T) {
	nvLinkDomainID := uuid.NewString()
	badRuleID := "not-a-uuid"
	tests := []struct {
		name    string
		request APIBatchNVLinkDomainFirmwareUpdateRequest
		wantErr bool
	}{
		{
			name: "accepts valid request",
			request: APIBatchNVLinkDomainFirmwareUpdateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID},
			},
		},
		{
			name: "rejects missing site",
			request: APIBatchNVLinkDomainFirmwareUpdateRequest{
				NVLinkDomainIDs: []string{nvLinkDomainID},
			},
			wantErr: true,
		},
		{
			name: "rejects malformed site",
			request: APIBatchNVLinkDomainFirmwareUpdateRequest{
				SiteID:          "not-a-uuid",
				NVLinkDomainIDs: []string{nvLinkDomainID},
			},
			wantErr: true,
		},
		{
			name:    "rejects missing domains",
			request: APIBatchNVLinkDomainFirmwareUpdateRequest{SiteID: uuid.NewString()},
			wantErr: true,
		},
		{
			name: "rejects invalid rule ID",
			request: APIBatchNVLinkDomainFirmwareUpdateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID},
				RuleID:          &badRuleID,
			},
			wantErr: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}
