// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestAPIExpectedMachineCreateRequest_Validate(t *testing.T) {
	emptyString := ""
	validChassisSerial := "CHASSIS123"
	validUsername := "admin"
	validPassword := "password123"

	tests := []struct {
		desc      string
		obj       APIExpectedMachineCreateRequest
		expectErr bool
	}{
		{
			desc: "ok when all required fields are provided",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: false,
		},
		{
			desc: "ok when required fields and optional fields are provided",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:            "00:11:22:33:44:55",
				DefaultBmcUsername:       &validUsername,
				DefaultBmcPassword:       &validPassword,
				ChassisSerialNumber:      validChassisSerial,
				FallbackDPUSerialNumbers: []string{"DPU001", "DPU002"},
				Labels:                   map[string]string{"env": "test", "zone": "us-west-1"},
			},
			expectErr: false,
		},
		{
			desc: "error when BmcMacAddress is missing",
			obj: APIExpectedMachineCreateRequest{
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when ChassisSerialNumber is missing",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
			},
			expectErr: true,
		},
		{
			desc: "error when BmcMacAddress is wrong length (too short)",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when ChassisSerialNumber is empty",
			obj: APIExpectedMachineCreateRequest{

				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: emptyString,
			},
			expectErr: true,
		},
		// Boundary tests for BMC username (max 16 characters)
		{
			desc: "ok when BMC username is exactly 16 characters",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  cutil.GetPtr(strings.Repeat("a", 16)),
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is 17 characters (over limit)",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  cutil.GetPtr(strings.Repeat("a", 17)),
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: true,
		},
		// Boundary tests for BMC password (max 20 characters)
		{
			desc: "ok when BMC password is exactly 20 characters",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  cutil.GetPtr(strings.Repeat("a", 20)),
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when BMC password is 21 characters (over limit)",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  cutil.GetPtr(strings.Repeat("a", 21)),
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: true,
		},
		// Boundary tests for chassis serial number (max 32 characters)
		{
			desc: "ok when chassis serial number is exactly 32 characters",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: strings.Repeat("a", 32),
			},
			expectErr: false,
		},
		{
			desc: "error when chassis serial number is 33 characters (over limit)",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: strings.Repeat("a", 33),
			},
			expectErr: true,
		},
		{
			desc: "ok when optional fields are empty",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:            "00:11:22:33:44:55",
				DefaultBmcUsername:       &emptyString,
				DefaultBmcPassword:       &emptyString,
				ChassisSerialNumber:      validChassisSerial,
				FallbackDPUSerialNumbers: []string{},
				Labels:                   map[string]string{},
			},
			expectErr: false,
		},
		{
			desc: "ok when SiteID is empty string",
			obj: APIExpectedMachineCreateRequest{
				SiteID:              "",
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: false,
		},
		{
			desc: "ok when SiteID is valid UUID",
			obj: APIExpectedMachineCreateRequest{
				SiteID:              "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is invalid UUID",
			obj: APIExpectedMachineCreateRequest{
				SiteID:              "not-a-valid-uuid",
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when SiteID is partially valid UUID",
			obj: APIExpectedMachineCreateRequest{
				SiteID:              "550e8400-e29b-41d4",
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
			},
			expectErr: true,
		},
		// BmcIpAddress validation tests
		{
			desc: "valid IPv4 BmcIpAddress",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("192.168.1.10"),
			},
			expectErr: false,
		},
		{
			desc: "valid IPv6 BmcIpAddress",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("2001:db8::1"),
			},
			expectErr: false,
		},
		{
			desc: "invalid BmcIpAddress",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("not-an-ip"),
			},
			expectErr: true,
		},
		{
			desc: "empty BmcIpAddress (pointer set, value empty)",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
				BmcIpAddress:        &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "nil BmcIpAddress (default)",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				ChassisSerialNumber: validChassisSerial,
				BmcIpAddress:        nil,
			},
			expectErr: false,
		},
		// HostLifecycleProfile validation tests
		{
			desc: "ok with hostLifecycleProfile disableLockdown true",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:        "00:11:22:33:44:55",
				DefaultBmcUsername:   &validUsername,
				DefaultBmcPassword:   &validPassword,
				ChassisSerialNumber:  validChassisSerial,
				HostLifecycleProfile: &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
			},
			expectErr: false,
		},
		{
			desc: "ok with hostLifecycleProfile disableLockdown false",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:        "00:11:22:33:44:55",
				DefaultBmcUsername:   &validUsername,
				DefaultBmcPassword:   &validPassword,
				ChassisSerialNumber:  validChassisSerial,
				HostLifecycleProfile: &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
			},
			expectErr: false,
		},
		{
			desc: "ok with hostLifecycleProfile present but disableLockdown unset",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:        "00:11:22:33:44:55",
				DefaultBmcUsername:   &validUsername,
				DefaultBmcPassword:   &validPassword,
				ChassisSerialNumber:  validChassisSerial,
				HostLifecycleProfile: &APIHostLifecycleProfile{},
			},
			expectErr: false,
		},
		{
			desc: "ok with nil hostLifecycleProfile",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:        "00:11:22:33:44:55",
				DefaultBmcUsername:   &validUsername,
				DefaultBmcPassword:   &validPassword,
				ChassisSerialNumber:  validChassisSerial,
				HostLifecycleProfile: nil,
			},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestNewAPIExpectedMachine(t *testing.T) {
	tests := []struct {
		desc                 string
		isDpfEnabled         *bool
		hostLifecycleProfile cdbm.HostLifecycleProfile
		wantDpfJSON          string
		wantProfile          *APIHostLifecycleProfile
		labels               cdbm.Labels
		dpuSerials           []string
		wantLabelsJSON       string
		wantSerialsJSON      string
	}{
		{
			desc:            "nil collections serialize as empty and unset DPF defaults to true",
			wantDpfJSON:     "true",
			wantLabelsJSON:  `{}`,
			wantSerialsJSON: `[]`,
		},
		{
			desc:            "empty collections remain empty",
			labels:          cdbm.Labels{},
			dpuSerials:      []string{},
			wantLabelsJSON:  `{}`,
			wantSerialsJSON: `[]`,
		},
		{
			desc:            "populated collections preserve values and order",
			labels:          cdbm.Labels{"env": "test", "zone": "us-west-1"},
			dpuSerials:      []string{"DPU002", "DPU001"},
			wantLabelsJSON:  `{"env":"test","zone":"us-west-1"}`,
			wantSerialsJSON: `["DPU002","DPU001"]`,
		},
		{
			desc:         "stored DPF false is returned",
			isDpfEnabled: cutil.GetPtr(false),
			wantDpfJSON:  "false",
		},
		{
			desc:         "stored DPF true is returned",
			isDpfEnabled: cutil.GetPtr(true),
			wantDpfJSON:  "true",
		},
		{
			desc:                 "disableLockdown true round-trips",
			hostLifecycleProfile: cdbm.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
			wantProfile:          &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		},
		{
			desc:                 "disableLockdown false round-trips",
			hostLifecycleProfile: cdbm.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
			wantProfile:          &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			dbEM := &cdbm.ExpectedMachine{
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: tc.dpuSerials,
				Labels:                   tc.labels,
				IsDpfEnabled:             tc.isDpfEnabled,
				HostLifecycleProfile:     tc.hostLifecycleProfile,
				Created:                  cdb.GetCurTime(),
				Updated:                  cdb.GetCurTime(),
			}
			stored := *dbEM

			got := NewAPIExpectedMachine(dbEM)

			assert.Equal(t, dbEM.BmcMacAddress, got.BmcMacAddress)
			assert.Equal(t, dbEM.ChassisSerialNumber, got.ChassisSerialNumber)
			assert.Equal(t, dbEM.Created, got.Created)
			assert.Equal(t, dbEM.Updated, got.Updated)
			assert.Equal(t, tc.wantProfile, got.HostLifecycleProfile)
			assert.Equal(t, stored, *dbEM, "response conversion must preserve stored nil values")

			raw, err := json.Marshal(got)
			require.NoError(t, err)
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(raw, &fields))
			if tc.wantLabelsJSON != "" {
				assert.JSONEq(t, tc.wantLabelsJSON, string(fields["labels"]))
				assert.JSONEq(t, tc.wantSerialsJSON, string(fields["fallbackDPUSerialNumbers"]))
			}
			if tc.wantDpfJSON != "" {
				assert.Equal(t, tc.wantDpfJSON, string(fields["isDpfEnabled"]))
			}
		})
	}
}

func TestAPIHostLifecycleProfile_Conversions(t *testing.T) {
	t.Run("ToDBModel maps disableLockdown", func(t *testing.T) {
		dbTrue := (&APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)}).ToDBModel()
		if assert.NotNil(t, dbTrue.DisableLockdown) {
			assert.Equal(t, true, *dbTrue.DisableLockdown)
		}
		dbFalse := (&APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)}).ToDBModel()
		if assert.NotNil(t, dbFalse.DisableLockdown) {
			assert.Equal(t, false, *dbFalse.DisableLockdown)
		}
	})

	t.Run("ToDBModel on nil receiver yields zero value", func(t *testing.T) {
		var p *APIHostLifecycleProfile
		assert.Nil(t, p.ToDBModel().DisableLockdown)
	})

	t.Run("ToDBModelPtr preserves nil-vs-set distinction", func(t *testing.T) {
		var p *APIHostLifecycleProfile
		assert.Nil(t, p.ToDBModelPtr())
		assert.Nil(t, (&APIHostLifecycleProfile{}).ToDBModelPtr())

		ptr := (&APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)}).ToDBModelPtr()
		if assert.NotNil(t, ptr) {
			assert.Equal(t, true, *ptr.DisableLockdown)
		}
	})

	t.Run("NewAPIHostLifecycleProfile omits unset profile", func(t *testing.T) {
		assert.Nil(t, NewAPIHostLifecycleProfile(cdbm.HostLifecycleProfile{}))

		got := NewAPIHostLifecycleProfile(cdbm.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)})
		if assert.NotNil(t, got) {
			assert.Equal(t, false, *got.DisableLockdown)
		}
	})
}

func TestAPIExpectedMachine_HostLifecycleProfile_JSONRoundTrip(t *testing.T) {
	t.Run("disableLockdown true survives marshal/unmarshal", func(t *testing.T) {
		orig := &APIExpectedMachine{
			BmcMacAddress:        "00:11:22:33:44:55",
			ChassisSerialNumber:  "CHASSIS123",
			HostLifecycleProfile: &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		}

		raw, err := json.Marshal(orig)
		assert.NoError(t, err)
		assert.Contains(t, string(raw), `"hostLifecycleProfile":{"disableLockdown":true}`)

		var round APIExpectedMachine
		assert.NoError(t, json.Unmarshal(raw, &round))
		if assert.NotNil(t, round.HostLifecycleProfile) {
			assert.Equal(t, true, *round.HostLifecycleProfile.DisableLockdown)
		}
	})

	t.Run("unset profile omitted from response JSON", func(t *testing.T) {
		raw, err := json.Marshal(&APIExpectedMachine{
			BmcMacAddress:       "00:11:22:33:44:55",
			ChassisSerialNumber: "CHASSIS123",
		})
		assert.NoError(t, err)
		assert.NotContains(t, string(raw), "hostLifecycleProfile")
	})
}

func TestAPIExpectedMachineUpdateRequest_BmcIpAddressJSONSemantics(t *testing.T) {
	tests := []struct {
		name               string
		body               string
		wantValue          *string
		wantUnmarshalError bool
		wantValidateError  bool
	}{
		{
			name: "omitted",
			body: `{}`,
		},
		{
			name: "explicit null",
			body: `{"bmcIpAddress":null}`,
		},
		{
			name:      "address",
			body:      `{"bmcIpAddress":"192.0.2.10"}`,
			wantValue: cutil.GetPtr("192.0.2.10"),
		},
		{
			name:      "empty string",
			body:      `{"bmcIpAddress":""}`,
			wantValue: cutil.GetPtr(""),
		},
		{
			name:               "wrong JSON type",
			body:               `{"bmcIpAddress":42}`,
			wantUnmarshalError: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var got APIExpectedMachineUpdateRequest
			err := json.Unmarshal([]byte(tc.body), &got)
			assert.Equal(t, tc.wantUnmarshalError, err != nil)
			if err != nil {
				return
			}

			assert.Equal(t, tc.wantValue, got.BmcIpAddress)
			assert.Equal(t, tc.wantValidateError, got.Validate() != nil)
		})
	}
}

func TestAPIExpectedMachineUpdateRequest_Validate(t *testing.T) {
	emptyString := ""
	validChassisSerial := "CHASSIS123"
	validUsername := "admin"
	validPassword := "password123"

	tests := []struct {
		desc      string
		obj       APIExpectedMachineUpdateRequest
		expectErr bool
	}{
		{
			desc: "ok when all fields are provided",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:      &validChassisSerial,
				FallbackDPUSerialNumbers: []string{"DPU001", "DPU002"},
				Labels:                   map[string]string{"env": "production", "zone": "us-east-1"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only chassis and labels are provided",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				Labels:              map[string]string{"team": "devops"},
			},
			expectErr: false,
		},
		{
			desc: "ok when chassis and fallback DPU serial numbers are provided",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:      &validChassisSerial,
				FallbackDPUSerialNumbers: []string{"DPU999"},
			},
			expectErr: false,
		},
		{
			desc: "ok when chassis provided with empty collections",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:      &validChassisSerial,
				FallbackDPUSerialNumbers: []string{},
				Labels:                   map[string]string{},
			},
			expectErr: false,
		},
		{
			desc: "ok when chassis serial number is not provided (nil)",
			obj: APIExpectedMachineUpdateRequest{
				FallbackDPUSerialNumbers: []string{"DPU001"},
				Labels:                   map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only labels are provided",
			obj: APIExpectedMachineUpdateRequest{
				Labels: map[string]string{"team": "devops"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only fallback DPU serial numbers are provided",
			obj: APIExpectedMachineUpdateRequest{
				FallbackDPUSerialNumbers: []string{"DPU999"},
			},
			expectErr: false,
		},
		{
			desc: "ok with nil values for all optional fields",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:      nil,
				FallbackDPUSerialNumbers: nil,
				Labels:                   nil,
			},
			expectErr: false,
		},
		{
			desc: "error when chassis serial number is empty string",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &emptyString,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "ok with many labels",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				Labels: map[string]string{
					"env":         "prod",
					"zone":        "us-west-2",
					"team":        "platform",
					"cost-center": "12345",
					"app":         "nico-rest-api",
				},
			},
			expectErr: false,
		},
		{
			desc: "ok with many DPU serial numbers",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				FallbackDPUSerialNumbers: []string{
					"DPU001", "DPU002", "DPU003", "DPU004", "DPU005",
					"DPU006", "DPU007", "DPU008", "DPU009", "DPU010",
				},
			},
			expectErr: false,
		},
		{
			desc: "ok with valid BMC credentials",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is empty string",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  &emptyString,
				DefaultBmcPassword:  &validPassword,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "error when BMC password is empty string",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcPassword:  &emptyString,
				DefaultBmcUsername:  &validUsername,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "error when both BMC username and password are empty strings",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  &emptyString,
				DefaultBmcPassword:  &emptyString,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "ok when BMC credentials are not provided (nil)",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  nil,
				DefaultBmcPassword:  nil,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		// Boundary tests for BMC username (max 16 characters)
		{
			desc: "ok when BMC username is exactly 16 characters",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  cutil.GetPtr(strings.Repeat("a", 16)),
				DefaultBmcPassword:  &validPassword,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is 17 characters (over limit)",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  cutil.GetPtr(strings.Repeat("a", 17)),
				DefaultBmcPassword:  &validPassword,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for BMC password (max 20 characters)
		{
			desc: "ok when BMC password is exactly 20 characters",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  cutil.GetPtr(strings.Repeat("a", 20)),
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC password is 21 characters (over limit)",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  cutil.GetPtr(strings.Repeat("a", 21)),
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for chassis serial number (max 32 characters)
		{
			desc: "ok when chassis serial number is exactly 32 characters",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: cutil.GetPtr(strings.Repeat("a", 32)),
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when chassis serial number is 33 characters (over limit)",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: cutil.GetPtr(strings.Repeat("a", 33)),
				DefaultBmcUsername:  &validUsername,
				DefaultBmcPassword:  &validPassword,
				Labels:              map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// BmcIpAddress validation tests
		{
			desc: "valid IPv4 BmcIpAddress",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("192.168.1.10"),
			},
			expectErr: false,
		},
		{
			desc: "valid IPv6 BmcIpAddress",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("2001:db8::1"),
			},
			expectErr: false,
		},
		{
			desc: "invalid BmcIpAddress",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("not-an-ip"),
			},
			expectErr: true,
		},
		{
			desc: "empty BmcIpAddress clears the value",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				BmcIpAddress:        &emptyString,
			},
			expectErr: false,
		},
		{
			desc: "nil BmcIpAddress (default)",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				BmcIpAddress:        nil,
			},
			expectErr: false,
		},
		// HostLifecycleProfile validation tests
		{
			desc: "ok with hostLifecycleProfile disableLockdown true",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:  &validChassisSerial,
				HostLifecycleProfile: &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
			},
			expectErr: false,
		},
		{
			desc: "ok with hostLifecycleProfile disableLockdown false",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:  &validChassisSerial,
				HostLifecycleProfile: &APIHostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
			},
			expectErr: false,
		},
		{
			desc: "ok with nil hostLifecycleProfile",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber:  &validChassisSerial,
				HostLifecycleProfile: nil,
			},
			expectErr: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestNewAPIExpectedMachineEdgeCases(t *testing.T) {
	t.Run("with empty strings in fields", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			BmcMacAddress:            "",
			ChassisSerialNumber:      "",
			FallbackDpuSerialNumbers: []string{""},
			Labels:                   map[string]string{"": ""},
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		got := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, got)
		assert.Equal(t, "", got.BmcMacAddress)
		assert.Equal(t, "", got.ChassisSerialNumber)
	})

	t.Run("with special characters in labels", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			BmcMacAddress:       "00:11:22:33:44:55",
			ChassisSerialNumber: "CHASSIS-123",
			Labels: map[string]string{
				"app.kubernetes.io/name":    "nico-rest-api",
				"app.kubernetes.io/version": "v1.2.3",
				"special-chars":             "value!@#$%^&*()",
			},
			Created: time.Now(),
			Updated: time.Now(),
		}

		got := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, got)
		assert.Equal(t, APILabels(dbEM.Labels), got.Labels)
		assert.Equal(t, "nico-rest-api", got.Labels["app.kubernetes.io/name"])
	})

	t.Run("with very long serial numbers", func(t *testing.T) {
		longSerial := "CHASSIS-" + string(make([]byte, 200))
		dbEM := &cdbm.ExpectedMachine{
			BmcMacAddress:       "00:11:22:33:44:55",
			ChassisSerialNumber: longSerial,
			Created:             time.Now(),
			Updated:             time.Now(),
		}

		got := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, got)
		assert.Equal(t, longSerial, got.ChassisSerialNumber)
	})

	t.Run("with many fallback DPU serial numbers", func(t *testing.T) {
		dpuSerials := make([]string, 100)
		for i := 0; i < 100; i++ {
			dpuSerials[i] = fmt.Sprintf("DPU-%03d", i)
		}

		dbEM := &cdbm.ExpectedMachine{
			BmcMacAddress:            "00:11:22:33:44:55",
			ChassisSerialNumber:      "CHASSIS123",
			FallbackDpuSerialNumbers: dpuSerials,
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		got := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, got)
		assert.Len(t, got.FallbackDPUSerialNumbers, 100)
		assert.Equal(t, "DPU-000", got.FallbackDPUSerialNumbers[0])
		assert.Equal(t, "DPU-099", got.FallbackDPUSerialNumbers[99])
	})

	t.Run("with unicode characters in labels", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			BmcMacAddress:       "00:11:22:33:44:55",
			ChassisSerialNumber: "CHASSIS123",
			Labels: map[string]string{
				"location": "東京",
				"owner":    "José García",
				"emoji":    "🚀🔥",
			},
			Created: time.Now(),
			Updated: time.Now(),
		}

		got := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, got)
		assert.Equal(t, "東京", got.Labels["location"])
		assert.Equal(t, "José García", got.Labels["owner"])
		assert.Equal(t, "🚀🔥", got.Labels["emoji"])
	})

	t.Run("with zero time values", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			BmcMacAddress:       "00:11:22:33:44:55",
			ChassisSerialNumber: "CHASSIS123",
			Created:             time.Time{},
			Updated:             time.Time{},
		}

		got := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, got)
		assert.True(t, got.Created.IsZero())
		assert.True(t, got.Updated.IsZero())
	})
}

func TestNewAPIExpectedMachineWithSkuAndSite(t *testing.T) {
	siteID := uuid.New()
	emID := uuid.New()
	skuID := "test-sku-id"

	t.Run("maps SkuID when provided", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			ID:                       emID,
			SiteID:                   siteID,
			BmcMacAddress:            "00:11:22:33:44:55",
			ChassisSerialNumber:      "CHASSIS123",
			FallbackDpuSerialNumbers: []string{},
			Labels:                   map[string]string{},
			SkuID:                    &skuID,
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		apiEM := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, apiEM)
		assert.NotNil(t, apiEM.SkuID)
		assert.Equal(t, skuID, *apiEM.SkuID)
	})

	t.Run("handles nil SkuID gracefully", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			ID:                       emID,
			SiteID:                   siteID,
			BmcMacAddress:            "00:11:22:33:44:55",
			ChassisSerialNumber:      "CHASSIS123",
			FallbackDpuSerialNumbers: []string{},
			Labels:                   map[string]string{},
			SkuID:                    nil,
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		apiEM := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, apiEM)
		assert.Nil(t, apiEM.SkuID)
	})

	t.Run("maps Site when provided", func(t *testing.T) {
		site := &cdbm.Site{
			ID:                       siteID,
			Name:                     "Test Site",
			Org:                      "test-org",
			InfrastructureProviderID: uuid.New(),
			IsSerialConsoleEnabled:   false,
			Status:                   "active",
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		dbEM := &cdbm.ExpectedMachine{
			ID:                       emID,
			SiteID:                   siteID,
			BmcMacAddress:            "00:11:22:33:44:55",
			ChassisSerialNumber:      "CHASSIS123",
			FallbackDpuSerialNumbers: []string{},
			Labels:                   map[string]string{},
			Site:                     site,
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		apiEM := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, apiEM)
		assert.NotNil(t, apiEM.Site)
		assert.Equal(t, siteID.String(), apiEM.Site.ID)
		assert.Equal(t, "Test Site", apiEM.Site.Name)
		assert.Equal(t, "test-org", apiEM.Site.Org)
	})

	t.Run("handles nil Site gracefully", func(t *testing.T) {
		dbEM := &cdbm.ExpectedMachine{
			ID:                       emID,
			SiteID:                   siteID,
			BmcMacAddress:            "00:11:22:33:44:55",
			ChassisSerialNumber:      "CHASSIS123",
			FallbackDpuSerialNumbers: []string{},
			Labels:                   map[string]string{},
			Site:                     nil,
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		apiEM := NewAPIExpectedMachine(dbEM)
		assert.NotNil(t, apiEM)
		assert.Nil(t, apiEM.Site)
	})
}

func TestNewAPIExpectedMachineWithSkuComponents(t *testing.T) {
	siteID := uuid.New()
	emID := uuid.New()

	tests := []struct {
		name     string
		dbEM     *cdbm.ExpectedMachine
		validate func(t *testing.T, apiEM *APIExpectedMachine)
	}{
		{
			name: "maps all SKU component types correctly",
			dbEM: &cdbm.ExpectedMachine{
				ID:                       emID,
				SiteID:                   siteID,
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: []string{"DPU001"},
				Labels:                   map[string]string{"env": "test"},
				Created:                  time.Now(),
				Updated:                  time.Now(),
				Sku: &cdbm.SKU{
					DeviceType:           cutil.GetPtr("gpu"),
					AssociatedMachineIds: []string{"machine-1", "machine-2"},
					Components: &cdbm.SkuComponents{
						SkuComponents: &corev1.SkuComponents{
							Cpus: []*corev1.SkuComponentCpu{
								{
									Vendor:      "Intel",
									Model:       "Xeon Gold 6354",
									ThreadCount: 72,
									Count:       2,
								},
							},
							Gpus: []*corev1.SkuComponentGpu{
								{
									Vendor:      "NVIDIA",
									Model:       "A100",
									TotalMemory: "80GB",
									Count:       8,
								},
							},
							Memory: []*corev1.SkuComponentMemory{
								{
									CapacityMb: 524288,
									Count:      16,
									MemoryType: "DDR4",
								},
							},
							Storage: []*corev1.SkuComponentStorage{
								{
									Vendor:     "Samsung",
									Model:      "PM9A3",
									CapacityMb: 3840000,
									Count:      4,
								},
							},
							Chassis: &corev1.SkuComponentChassis{
								Vendor: "Dell",
								Model:  "PowerEdge R750xa",
							},
							Tpm: &corev1.SkuComponentTpm{
								Vendor:  "Infineon",
								Version: "2.0",
							},
						},
					},
				},
			},
			validate: func(t *testing.T, apiEM *APIExpectedMachine) {
				assert.NotNil(t, apiEM.Sku)
				assert.NotNil(t, apiEM.Sku.DeviceType)
				assert.Equal(t, "gpu", *apiEM.Sku.DeviceType)
				assert.Equal(t, []string{"machine-1", "machine-2"}, apiEM.Sku.AssociatedMachineIds)

				// Validate SKU has components
				assert.NotNil(t, apiEM.Sku.Components)

				// Validate CPU components
				assert.Len(t, apiEM.Sku.Components.Cpus, 1)
				assert.Equal(t, "Intel", apiEM.Sku.Components.Cpus[0].Vendor)
				assert.Equal(t, "Xeon Gold 6354", apiEM.Sku.Components.Cpus[0].Model)
				assert.Equal(t, uint32(72), apiEM.Sku.Components.Cpus[0].ThreadCount)
				assert.Equal(t, uint32(2), apiEM.Sku.Components.Cpus[0].Count)

				// Validate GPU components
				assert.Len(t, apiEM.Sku.Components.Gpus, 1)
				assert.Equal(t, "NVIDIA", apiEM.Sku.Components.Gpus[0].Vendor)
				assert.Equal(t, "A100", apiEM.Sku.Components.Gpus[0].Model)
				assert.Equal(t, "80GB", apiEM.Sku.Components.Gpus[0].TotalMemory)
				assert.Equal(t, uint32(8), apiEM.Sku.Components.Gpus[0].Count)

				// Validate Memory components
				assert.Len(t, apiEM.Sku.Components.Memory, 1)
				assert.Equal(t, uint32(524288), apiEM.Sku.Components.Memory[0].CapacityMb)
				assert.Equal(t, uint32(16), apiEM.Sku.Components.Memory[0].Count)
				assert.Equal(t, "DDR4", apiEM.Sku.Components.Memory[0].MemoryType)

				// Validate Storage components
				assert.Len(t, apiEM.Sku.Components.Storage, 1)
				assert.Equal(t, cutil.GetPtr("Samsung"), apiEM.Sku.Components.Storage[0].Vendor)
				assert.Equal(t, "PM9A3", apiEM.Sku.Components.Storage[0].Model)
				assert.Equal(t, cutil.GetPtr(uint32(3840000)), apiEM.Sku.Components.Storage[0].CapacityMb)
				assert.Equal(t, uint32(4), apiEM.Sku.Components.Storage[0].Count)

				// Validate Chassis component
				assert.NotNil(t, apiEM.Sku.Components.Chassis)
				assert.Equal(t, "Dell", apiEM.Sku.Components.Chassis.Vendor)
				assert.Equal(t, "PowerEdge R750xa", apiEM.Sku.Components.Chassis.Model)

				// Validate Tpm components
				assert.NotNil(t, apiEM.Sku.Components.Tpm)
				assert.Equal(t, "Infineon", apiEM.Sku.Components.Tpm.Vendor)
				assert.Equal(t, "2.0", apiEM.Sku.Components.Tpm.Version)
			},
		},
		{
			name: "handles nil SKU gracefully",
			dbEM: &cdbm.ExpectedMachine{
				ID:                       emID,
				SiteID:                   siteID,
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: []string{},
				Labels:                   map[string]string{},
				Created:                  time.Now(),
				Updated:                  time.Now(),
				Sku:                      nil,
			},
			validate: func(t *testing.T, apiEM *APIExpectedMachine) {
				assert.Nil(t, apiEM.Sku)
			},
		},
		{
			name: "handles nil SKU Components gracefully",
			dbEM: &cdbm.ExpectedMachine{
				ID:                       emID,
				SiteID:                   siteID,
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: []string{},
				Labels:                   map[string]string{},
				Created:                  time.Now(),
				Updated:                  time.Now(),
				Sku: &cdbm.SKU{
					DeviceType:           cutil.GetPtr("cpu"),
					AssociatedMachineIds: []string{},
					Components:           nil,
				},
			},
			validate: func(t *testing.T, apiEM *APIExpectedMachine) {
				assert.NotNil(t, apiEM.Sku)
				assert.Nil(t, apiEM.Sku.Components)
				assert.NotNil(t, apiEM.Sku.DeviceType)
				assert.Equal(t, "cpu", *apiEM.Sku.DeviceType)
			},
		},
		{
			name: "handles empty SKU Components gracefully",
			dbEM: &cdbm.ExpectedMachine{
				ID:                       emID,
				SiteID:                   siteID,
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: []string{},
				Labels:                   map[string]string{},
				Created:                  time.Now(),
				Updated:                  time.Now(),
				Sku: &cdbm.SKU{
					DeviceType:           cutil.GetPtr("storage"),
					AssociatedMachineIds: []string{},
					Components: &cdbm.SkuComponents{
						SkuComponents: &corev1.SkuComponents{},
					},
				},
			},
			validate: func(t *testing.T, apiEM *APIExpectedMachine) {
				assert.NotNil(t, apiEM.Sku)
				assert.NotNil(t, apiEM.Sku.Components)
				assert.Empty(t, apiEM.Sku.Components.Cpus)
				assert.Empty(t, apiEM.Sku.Components.Gpus)
				assert.Empty(t, apiEM.Sku.Components.Memory)
				assert.Empty(t, apiEM.Sku.Components.Storage)
				assert.Nil(t, apiEM.Sku.Components.Chassis)
				assert.Empty(t, apiEM.Sku.Components.EthernetDevices)
				assert.Empty(t, apiEM.Sku.Components.InfinibandDevices)
				assert.Empty(t, apiEM.Sku.Components.Tpm)
			},
		},
		{
			name: "maps multiple components of same type",
			dbEM: &cdbm.ExpectedMachine{
				ID:                       emID,
				SiteID:                   siteID,
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: []string{},
				Labels:                   map[string]string{},
				Created:                  time.Now(),
				Updated:                  time.Now(),
				Sku: &cdbm.SKU{
					Components: &cdbm.SkuComponents{
						SkuComponents: &corev1.SkuComponents{
							Gpus: []*corev1.SkuComponentGpu{
								{
									Vendor:      "NVIDIA",
									Model:       "A100",
									TotalMemory: "80GB",
									Count:       4,
								},
								{
									Vendor:      "NVIDIA",
									Model:       "H100",
									TotalMemory: "80GB",
									Count:       4,
								},
							},
							Storage: []*corev1.SkuComponentStorage{
								{
									Vendor:     "Samsung",
									Model:      "PM9A3",
									CapacityMb: 3840000,
									Count:      2,
								},
								{
									Vendor:     "Intel",
									Model:      "P5520",
									CapacityMb: 7680000,
									Count:      2,
								},
							},
						},
					},
				},
			},
			validate: func(t *testing.T, apiEM *APIExpectedMachine) {
				assert.NotNil(t, apiEM.Sku)
				assert.NotNil(t, apiEM.Sku.Components)

				// Validate multiple GPU components
				assert.Len(t, apiEM.Sku.Components.Gpus, 2)
				assert.Equal(t, "A100", apiEM.Sku.Components.Gpus[0].Model)
				assert.Equal(t, "H100", apiEM.Sku.Components.Gpus[1].Model)

				// Validate multiple Storage components
				assert.Len(t, apiEM.Sku.Components.Storage, 2)
				assert.Equal(t, cutil.GetPtr("Samsung"), apiEM.Sku.Components.Storage[0].Vendor)
				assert.Equal(t, cutil.GetPtr("Intel"), apiEM.Sku.Components.Storage[1].Vendor)
			},
		},
		{
			name: "handles partial SKU Components",
			dbEM: &cdbm.ExpectedMachine{
				ID:                       emID,
				SiteID:                   siteID,
				BmcMacAddress:            "00:11:22:33:44:55",
				ChassisSerialNumber:      "CHASSIS123",
				FallbackDpuSerialNumbers: []string{},
				Labels:                   map[string]string{},
				Created:                  time.Now(),
				Updated:                  time.Now(),
				Sku: &cdbm.SKU{
					Components: &cdbm.SkuComponents{
						SkuComponents: &corev1.SkuComponents{
							Cpus: []*corev1.SkuComponentCpu{
								{
									Vendor:      "AMD",
									Model:       "EPYC 7763",
									ThreadCount: 128,
									Count:       2,
								},
							},
							Chassis: &corev1.SkuComponentChassis{
								Vendor: "HPE",
								Model:  "ProLiant DL380",
							},
							// Only CPU and Chassis, other components are nil/empty
						},
					},
				},
			},
			validate: func(t *testing.T, apiEM *APIExpectedMachine) {
				assert.NotNil(t, apiEM.Sku)
				assert.NotNil(t, apiEM.Sku.Components)

				// Validate present components
				assert.Len(t, apiEM.Sku.Components.Cpus, 1)
				assert.Equal(t, "AMD", apiEM.Sku.Components.Cpus[0].Vendor)

				assert.NotNil(t, apiEM.Sku.Components.Chassis)
				assert.Equal(t, "HPE", apiEM.Sku.Components.Chassis.Vendor)

				// Validate absent components are empty
				assert.Empty(t, apiEM.Sku.Components.Gpus)
				assert.Empty(t, apiEM.Sku.Components.Memory)
				assert.Empty(t, apiEM.Sku.Components.Storage)
				assert.Empty(t, apiEM.Sku.Components.EthernetDevices)
				assert.Empty(t, apiEM.Sku.Components.InfinibandDevices)
				assert.Empty(t, apiEM.Sku.Components.Tpm)
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			apiEM := NewAPIExpectedMachine(tc.dbEM)
			assert.NotNil(t, apiEM)

			// Validate basic fields
			assert.Equal(t, tc.dbEM.BmcMacAddress, apiEM.BmcMacAddress)
			assert.Equal(t, tc.dbEM.ChassisSerialNumber, apiEM.ChassisSerialNumber)

			// Run custom validation
			tc.validate(t, apiEM)
		})
	}
}

func TestAPIExpectedMachineUpdateRequest_ToProto(t *testing.T) {
	tests := []struct {
		name      string
		body      string
		wantPaths []string
	}{
		{name: "omitted fields preserve Core state", body: `{}`},
		{name: "null fields preserve Core state", body: `{"defaultBmcUsername":null,"defaultBmcPassword":null,"labels":null,"slotId":null,"bmcIpAddress":null}`},
		{name: "explicit zero and empty values remain selected", body: `{"slotId":0,"labels":{},"fallbackDPUSerialNumbers":[],"isDpfEnabled":false,"hostLifecycleProfile":{"disableLockdown":false},"bmcIpAddress":""}`, wantPaths: []string{"bmc_ip_address", "metadata.labels", "fallback_dpu_serial_numbers", "is_dpf_enabled", "host_lifecycle_profile.disable_lockdown"}},
		{name: "BMC address selects automatic allocation", body: `{"bmcIpAddress":"192.0.2.31"}`, wantPaths: []string{"bmc_ip_address", "bmc_ip_allocation"}},
		{name: "slot ID alone selects derived labels", body: `{"slotId":0}`, wantPaths: []string{"metadata.labels"}},
		{name: "BMC username leaves the password unselected", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":null}`, wantPaths: []string{"bmc_username"}},
		{name: "BMC password leaves the username unselected", body: `{"defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_password"}},
		{name: "BMC pair is selected together", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_username", "bmc_password"}},
		{name: "empty lifecycle profile preserves policy", body: `{"hostLifecycleProfile":{}}`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var request APIExpectedMachineUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(test.body), &request))
			require.NoError(t, request.Validate())
			patch := request.ToProto(&cdbm.ExpectedMachine{SlotID: cutil.GetPtr(int32(0)), IsDpfEnabled: cutil.GetPtr(false), HostLifecycleProfile: cdbm.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)}})
			encoded, err := protojson.Marshal(patch)
			require.NoError(t, err)
			var decoded corev1.PatchExpectedMachineRequest
			require.NoError(t, protojson.Unmarshal(encoded, &decoded))
			require.NotNil(t, decoded.UpdateMask)
			assert.Equal(t, test.wantPaths, decoded.GetUpdateMask().GetPaths())
			if request.SlotID != nil {
				labels := decoded.GetExpectedMachine().GetMetadata().GetLabels()
				require.Len(t, labels, 1)
				assert.Equal(t, "slot_id", labels[0].GetKey())
				assert.Equal(t, "0", labels[0].GetValue())
			}
			if request.DefaultBmcUsername != nil {
				assert.Equal(t, *request.DefaultBmcUsername, decoded.GetExpectedMachine().GetBmcUsername())
			}
			if request.DefaultBmcPassword != nil {
				assert.Equal(t, *request.DefaultBmcPassword, decoded.GetExpectedMachine().GetBmcPassword())
			}
			assert.Equal(t, request.BmcIpAddress, decoded.GetExpectedMachine().BmcIpAddress)
			if request.BmcIpAddress != nil && *request.BmcIpAddress != "" {
				assert.Equal(t, corev1.BmcIpAllocationType_BMC_IP_ALLOCATION_TYPE_AUTO, decoded.GetExpectedMachine().GetBmcIpAllocation())
			}
		})
	}
}
