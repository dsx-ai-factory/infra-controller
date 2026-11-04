// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
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
		{name: "accepts external ID", nvLinkDomainID: "rack-01"},
		{name: "rejects whitespace", nvLinkDomainID: " ", wantErr: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := ValidateNVLinkDomainID(test.nvLinkDomainID)
			if test.wantErr {
				require.EqualError(t, err, "NVLink Domain ID must not be blank")
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPINVLinkDomain_FromProto(t *testing.T) {
	tests := []struct{ name, topology, domainName string }{
		{name: "Flow topology preserved", topology: "CUSTOM_TOPOLOGY_NVIDIA", domainName: "nvl5-gp1"},
		{name: "unavailable"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := &flowv1.NVLinkDomain{
				Id: "group-01", RackGroupId: "group-01", NmxcClusterId: new("59202b81-65fb-45ec-b3b8-91ab0ad3f34a"), Name: tc.domainName,
				OperationStatus: flowv1.Phase_PHASE_READY,
			}
			if tc.topology != "" {
				r.Topology = &tc.topology
			}
			d := &APINVLinkDomain{}
			d.FromProto(r, true)
			assert.Equal(t, "group-01", d.ID)
			assert.Equal(t, d.ID, d.RackGroupID)
			assert.Equal(t, r.NmxcClusterId, d.NMXCClusterID)
			assert.Equal(t, tc.domainName, d.Name)
			assert.Equal(t, "Ready", d.OperationStatus)
			assert.NotNil(t, d.Components)
			if tc.topology == "" {
				assert.Nil(t, d.Topology)
			} else {
				require.NotNil(t, d.Topology)
				assert.Equal(t, tc.topology, *d.Topology)
			}
			data, err := json.Marshal(d)
			require.NoError(t, err)
			assert.Contains(t, string(data), `"components":[]`)
			assert.Contains(t, string(data), `"topology":`)
			if tc.domainName == "" {
				assert.Contains(t, string(data), `"name":""`)
			}
		})
	}
}

func TestNewAPINVLinkDomain(t *testing.T) {
	for _, tc := range []struct {
		name              string
		domain            *flowv1.NVLinkDomain
		includeComponents bool
		wantJSON          string
	}{
		{name: "nil domain"},
		{name: "not requested", domain: &flowv1.NVLinkDomain{Components: []*flowv1.Component{{}}}, wantJSON: `"components":null`},
		{name: "requested empty", domain: &flowv1.NVLinkDomain{}, includeComponents: true, wantJSON: `"components":[]`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := NewAPINVLinkDomain(tc.domain, tc.includeComponents)
			if tc.domain == nil {
				assert.Nil(t, got)
				return
			}
			require.NotNil(t, got)
			data, err := json.Marshal(got)
			require.NoError(t, err)
			assert.Contains(t, string(data), tc.wantJSON)
		})
	}
}

func TestNVLinkDomainTargetSpec(t *testing.T) {
	nvLinkDomainIDs := []string{uuid.NewString(), uuid.NewString()}

	got := NVLinkDomainTargetSpec(nvLinkDomainIDs)
	targets := got.GetNvlDomains().GetTargets()

	require.Len(t, targets, 2)
	assert.Equal(t, nvLinkDomainIDs[0], targets[0].GetExternalId())
	assert.Equal(t, nvLinkDomainIDs[1], targets[1].GetExternalId())
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
		wantState string
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
			wantState: PowerControlStateOn,
		},
		{
			name: "normalizes legacy power state",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID},
				State:           "acpowercycle",
			},
			wantState: PowerControlStateACCycle,
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
			name: "rejects blank domain ID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{" "},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("0: NVLink Domain ID must not be blank"),
		},
		{
			name: "rejects empty domain ID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{""},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("0: NVLink Domain ID must not be blank"),
		},
		{
			name: "rejects duplicate domain ID",
			request: APIBatchUpdateNVLinkDomainPowerStateRequest{
				SiteID:          uuid.NewString(),
				NVLinkDomainIDs: []string{nvLinkDomainID, nvLinkDomainID},
				State:           PowerControlStateOn,
			},
			assertErr: errorContains("1: duplicates NVLink Domain ID " + nvLinkDomainID),
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
				NVLinkDomainIDs: []string{" "},
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
					"domainIds":{"0":"NVLink Domain ID must not be blank"},
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
			if test.wantState != "" {
				assert.Equal(t, test.wantState, test.request.State)
			}
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
