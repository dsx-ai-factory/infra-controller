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

func TestAPIExpectedSwitchCreateRequest_Validate(t *testing.T) {
	emptyString := ""
	validSwitchSerial := "SWITCH123"
	validUsername := "admin"
	validPassword := "password123"

	tests := []struct {
		desc      string
		obj       APIExpectedSwitchCreateRequest
		expectErr bool
	}{
		{
			desc: "ok when all required fields are provided",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: false,
		},
		{
			desc: "ok when required fields and optional fields are provided",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvOsUsername:       cutil.GetPtr("nvosadmin"),
				NvOsPassword:       cutil.GetPtr("nvospass"),
				Labels:             map[string]string{"env": "test", "zone": "us-west-1"},
			},
			expectErr: false,
		},
		{
			desc: "error when BmcMacAddress is missing",
			obj: APIExpectedSwitchCreateRequest{
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when SwitchSerialNumber is missing",
			obj: APIExpectedSwitchCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
			},
			expectErr: true,
		},
		{
			desc: "error when BmcMacAddress is wrong length (too short)",
			obj: APIExpectedSwitchCreateRequest{
				BmcMacAddress:      "00:11:22:33:44",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when SwitchSerialNumber is empty",
			obj: APIExpectedSwitchCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: emptyString,
			},
			expectErr: true,
		},
		// Boundary tests for BMC username (max 16 characters)
		{
			desc: "ok when BMC username is exactly 16 characters",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 16)),
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is 17 characters (over limit)",
			obj: APIExpectedSwitchCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 17)),
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		// Boundary tests for BMC password (max 20 characters)
		{
			desc: "ok when BMC password is exactly 20 characters",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 20)),
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when BMC password is 21 characters (over limit)",
			obj: APIExpectedSwitchCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 21)),
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		// Boundary tests for switch serial number (max 32 characters)
		{
			desc: "ok when switch serial number is exactly 32 characters",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: strings.Repeat("a", 32),
			},
			expectErr: false,
		},
		{
			desc: "error when switch serial number is 33 characters (over limit)",
			obj: APIExpectedSwitchCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: strings.Repeat("a", 33),
			},
			expectErr: true,
		},
		{
			desc: "ok when optional fields are empty",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &emptyString,
				DefaultBmcPassword: &emptyString,
				SwitchSerialNumber: validSwitchSerial,
				Labels:             map[string]string{},
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is empty string",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		{
			desc: "ok when SiteID is valid UUID",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is invalid UUID",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "not-a-valid-uuid",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when SiteID is partially valid UUID",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
			},
			expectErr: true,
		},
		// BmcIpAddress validation tests
		{
			desc: "valid IPv4 BmcIpAddress",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				BmcIpAddress:       cutil.GetPtr("192.168.1.10"),
			},
			expectErr: false,
		},
		{
			desc: "valid IPv6 BmcIpAddress",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				BmcIpAddress:       cutil.GetPtr("2001:db8::1"),
			},
			expectErr: false,
		},
		{
			desc: "invalid BmcIpAddress",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				BmcIpAddress:       cutil.GetPtr("not-an-ip"),
			},
			expectErr: true,
		},
		{
			desc: "empty BmcIpAddress (pointer set, value empty)",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				BmcIpAddress:       &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "nil BmcIpAddress (default)",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				BmcIpAddress:       nil,
			},
			expectErr: false,
		},
		// NvosMacAddresses validation tests
		{
			desc: "ok when NvosMacAddresses entries are valid MACs",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "00:11:22:33:44:77"},
			},
			expectErr: false,
		},
		{
			desc: "error when NvosMacAddresses entry is not a MAC",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "not-a-mac"},
			},
			expectErr: true,
		},
		{
			desc: "error when NvosMacAddresses entry is empty",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{""},
			},
			expectErr: true,
		},
		{
			desc: "ok when NvosMacAddresses is an empty list",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{},
			},
			expectErr: false,
		},
		{
			desc: "error when NvosMacAddresses has duplicate entries",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "00:11:22:33:44:66"},
			},
			expectErr: true,
		},
		{
			desc: "error when NvosMacAddresses entries duplicate across separator styles",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "00-11-22-33-44-66"},
			},
			expectErr: true,
		},
		{
			desc: "error when NvosMacAddresses entry is not in the published 6-octet format",
			obj: APIExpectedSwitchCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				SwitchSerialNumber: validSwitchSerial,
				NvosMacAddresses:   []string{"0011.2233.4455"},
			},
			expectErr: true,
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

func TestNewAPIExpectedSwitch(t *testing.T) {
	bmcIP := "192.168.1.10"
	dbES := &cdbm.ExpectedSwitch{
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH123",
		BmcIpAddress:       &bmcIP,
		NvosMacAddresses:   []string{"00:11:22:33:44:66", "00:11:22:33:44:77"},
		Labels:             map[string]string{"env": "test", "zone": "us-west-1"},
		Created:            cdb.GetCurTime(),
		Updated:            cdb.GetCurTime(),
	}

	tests := []struct {
		desc  string
		dbObj *cdbm.ExpectedSwitch
	}{
		{
			desc:  "test creating API ExpectedSwitch",
			dbObj: dbES,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIExpectedSwitch(tc.dbObj)

			// Verify all fields are properly mapped
			assert.Equal(t, tc.dbObj.BmcMacAddress, got.BmcMacAddress)
			assert.Equal(t, tc.dbObj.SwitchSerialNumber, got.SwitchSerialNumber)
			assert.Equal(t, tc.dbObj.BmcIpAddress, got.BmcIpAddress)
			assert.Equal(t, APINvosMacAddresses(tc.dbObj.NvosMacAddresses), got.NvosMacAddresses)
			assert.Equal(t, APILabels(tc.dbObj.Labels), got.Labels)
			assert.Equal(t, tc.dbObj.Created, got.Created)
			assert.Equal(t, tc.dbObj.Updated, got.Updated)
		})
	}
}

func TestNewAPIExpectedSwitchWithNilFields(t *testing.T) {
	dbES := &cdbm.ExpectedSwitch{
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH123",
		Labels:             nil,
		Created:            time.Now(),
		Updated:            time.Now(),
	}

	got := NewAPIExpectedSwitch(dbES)

	// Verify fields are properly handled when empty or nil
	assert.Equal(t, dbES.BmcMacAddress, got.BmcMacAddress)
	assert.Equal(t, dbES.SwitchSerialNumber, got.SwitchSerialNumber)
	assert.Nil(t, got.Labels)
}

func TestAPIExpectedSwitchUpdateRequest_Validate(t *testing.T) {
	emptyString := ""
	validSwitchSerial := "SWITCH123"
	validUsername := "admin"
	validPassword := "password123"

	tests := []struct {
		desc      string
		obj       APIExpectedSwitchUpdateRequest
		expectErr bool
	}{
		{
			desc:      "error when only DefaultBmcUsername is provided",
			obj:       APIExpectedSwitchUpdateRequest{DefaultBmcUsername: cutil.GetPtr("partial-pair")},
			expectErr: true,
		},
		{
			desc:      "error when only DefaultBmcPassword is provided",
			obj:       APIExpectedSwitchUpdateRequest{DefaultBmcPassword: cutil.GetPtr("partial-pair")},
			expectErr: true,
		},
		{
			desc:      "NVOS username without password",
			obj:       APIExpectedSwitchUpdateRequest{NvOsUsername: cutil.GetPtr("admin")},
			expectErr: true,
		},
		{
			desc:      "NVOS password without username",
			obj:       APIExpectedSwitchUpdateRequest{NvOsPassword: cutil.GetPtr("secret")},
			expectErr: true,
		},
		{
			desc:      "empty NVOS pair",
			obj:       APIExpectedSwitchUpdateRequest{NvOsUsername: cutil.GetPtr(""), NvOsPassword: cutil.GetPtr("")},
			expectErr: true,
		},
		{
			desc:      "valid NVOS pair without BMC update",
			obj:       APIExpectedSwitchUpdateRequest{NvOsUsername: cutil.GetPtr("admin"), NvOsPassword: cutil.GetPtr("secret")},
			expectErr: false,
		},
		{
			desc: "ok when all fields are provided",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				Labels:             map[string]string{"env": "production", "zone": "us-east-1"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only switch serial and labels are provided",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				Labels:             map[string]string{"team": "devops"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only labels are provided",
			obj: APIExpectedSwitchUpdateRequest{
				Labels: map[string]string{"team": "devops"},
			},
			expectErr: false,
		},
		{
			desc: "ok with nil values for all optional fields",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: nil,
				Labels:             nil,
			},
			expectErr: false,
		},
		{
			desc: "error when switch serial number is empty string",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &emptyString,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "ok with valid BMC credentials",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is empty string",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcUsername: &emptyString,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "error when BMC password is empty string",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcPassword: &emptyString,
				DefaultBmcUsername: &validUsername,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for BMC username (max 16 characters)
		{
			desc: "ok when BMC username is exactly 16 characters",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 16)),
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is 17 characters (over limit)",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 17)),
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for BMC password (max 20 characters)
		{
			desc: "ok when BMC password is exactly 20 characters",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 20)),
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC password is 21 characters (over limit)",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 21)),
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for switch serial number (max 32 characters)
		{
			desc: "ok when switch serial number is exactly 32 characters",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: cutil.GetPtr(strings.Repeat("a", 32)),
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when switch serial number is 33 characters (over limit)",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: cutil.GetPtr(strings.Repeat("a", 33)),
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// BmcIpAddress validation tests
		{
			desc: "valid IPv4 BmcIpAddress",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				BmcIpAddress:       cutil.GetPtr("192.168.1.10"),
			},
			expectErr: false,
		},
		{
			desc: "valid IPv6 BmcIpAddress",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				BmcIpAddress:       cutil.GetPtr("2001:db8::1"),
			},
			expectErr: false,
		},
		{
			desc: "invalid BmcIpAddress",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				BmcIpAddress:       cutil.GetPtr("not-an-ip"),
			},
			expectErr: true,
		},
		{
			desc: "empty BmcIpAddress (pointer set, value empty)",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				BmcIpAddress:       &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "nil BmcIpAddress (default)",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				BmcIpAddress:       nil,
			},
			expectErr: false,
		},
		// NvosMacAddresses validation tests
		{
			desc: "ok when NvosMacAddresses entries are valid MACs",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "00:11:22:33:44:77"},
			},
			expectErr: false,
		},
		{
			desc: "error when NvosMacAddresses entry is not a MAC",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{"not-a-mac"},
			},
			expectErr: true,
		},
		{
			desc: "error when NvosMacAddresses entry is empty",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{""},
			},
			expectErr: true,
		},
		{
			desc: "ok when NvosMacAddresses is an empty list",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{},
			},
			expectErr: false,
		},
		{
			desc: "error when NvosMacAddresses has duplicate entries",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "00:11:22:33:44:66"},
			},
			expectErr: true,
		},
		{
			desc: "error when NvosMacAddresses entries duplicate across separator styles",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{"00:11:22:33:44:66", "00-11-22-33-44-66"},
			},
			expectErr: true,
		},
		{
			desc: "error when NvosMacAddresses entry is not in the published 6-octet format",
			obj: APIExpectedSwitchUpdateRequest{
				SwitchSerialNumber: &validSwitchSerial,
				NvosMacAddresses:   []string{"0011.2233.4455"},
			},
			expectErr: true,
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

func TestNewAPIExpectedSwitchEdgeCases(t *testing.T) {
	t.Run("with empty strings in fields", func(t *testing.T) {
		dbES := &cdbm.ExpectedSwitch{
			BmcMacAddress:      "",
			SwitchSerialNumber: "",
			Labels:             map[string]string{"": ""},
			Created:            time.Now(),
			Updated:            time.Now(),
		}

		got := NewAPIExpectedSwitch(dbES)
		assert.NotNil(t, got)
		assert.Equal(t, "", got.BmcMacAddress)
		assert.Equal(t, "", got.SwitchSerialNumber)
	})

	t.Run("with special characters in labels", func(t *testing.T) {
		dbES := &cdbm.ExpectedSwitch{
			BmcMacAddress:      "00:11:22:33:44:55",
			SwitchSerialNumber: "SWITCH-123",
			Labels: map[string]string{
				"app.kubernetes.io/name":    "cloud-api",
				"app.kubernetes.io/version": "v1.2.3",
				"special-chars":             "value!@#$%^&*()",
			},
			Created: time.Now(),
			Updated: time.Now(),
		}

		got := NewAPIExpectedSwitch(dbES)
		assert.NotNil(t, got)
		assert.Equal(t, APILabels(dbES.Labels), got.Labels)
		assert.Equal(t, "cloud-api", got.Labels["app.kubernetes.io/name"])
	})

	t.Run("with unicode characters in labels", func(t *testing.T) {
		dbES := &cdbm.ExpectedSwitch{
			BmcMacAddress:      "00:11:22:33:44:55",
			SwitchSerialNumber: "SWITCH123",
			Labels: map[string]string{
				"location": "東京",
				"owner":    "José García",
				"emoji":    "🚀🔥",
			},
			Created: time.Now(),
			Updated: time.Now(),
		}

		got := NewAPIExpectedSwitch(dbES)
		assert.NotNil(t, got)
		assert.Equal(t, "東京", got.Labels["location"])
		assert.Equal(t, "José García", got.Labels["owner"])
		assert.Equal(t, "🚀🔥", got.Labels["emoji"])
	})

	t.Run("with zero time values", func(t *testing.T) {
		dbES := &cdbm.ExpectedSwitch{
			BmcMacAddress:      "00:11:22:33:44:55",
			SwitchSerialNumber: "SWITCH123",
			Created:            time.Time{},
			Updated:            time.Time{},
		}

		got := NewAPIExpectedSwitch(dbES)
		assert.NotNil(t, got)
		assert.True(t, got.Created.IsZero())
		assert.True(t, got.Updated.IsZero())
	})
}

func TestNewAPIExpectedSwitchWithSite(t *testing.T) {
	siteID := uuid.New()
	esID := uuid.New()

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

		dbES := &cdbm.ExpectedSwitch{
			ID:                 esID,
			SiteID:             siteID,
			BmcMacAddress:      "00:11:22:33:44:55",
			SwitchSerialNumber: "SWITCH123",
			Labels:             map[string]string{},
			Site:               site,
			Created:            time.Now(),
			Updated:            time.Now(),
		}

		apiES := NewAPIExpectedSwitch(dbES)
		assert.NotNil(t, apiES)
		assert.NotNil(t, apiES.Site)
		assert.Equal(t, siteID.String(), apiES.Site.ID)
		assert.Equal(t, "Test Site", apiES.Site.Name)
		assert.Equal(t, "test-org", apiES.Site.Org)
	})

	t.Run("handles nil Site gracefully", func(t *testing.T) {
		dbES := &cdbm.ExpectedSwitch{
			ID:                 esID,
			SiteID:             siteID,
			BmcMacAddress:      "00:11:22:33:44:55",
			SwitchSerialNumber: "SWITCH123",
			Labels:             map[string]string{},
			Site:               nil,
			Created:            time.Now(),
			Updated:            time.Now(),
		}

		apiES := NewAPIExpectedSwitch(dbES)
		assert.NotNil(t, apiES)
		assert.Nil(t, apiES.Site)
	})
}

func TestAPIExpectedSwitchUpdateRequest_ToProto(t *testing.T) {
	tests := []struct {
		name      string
		body      string
		wantPaths []string
	}{
		{name: "omitted fields preserve Core state", body: `{}`},
		{name: "null fields preserve Core state", body: `{"defaultBmcUsername":null,"defaultBmcPassword":null,"nvOsUsername":null,"nvOsPassword":null,"labels":null}`},
		{name: "explicit zero and empty values remain selected", body: `{"slotId":0,"labels":{},"nvosMacAddresses":[]}`, wantPaths: []string{"metadata.labels", "nvos_mac_addresses"}},
		{name: "slot ID alone selects derived labels", body: `{"slotId":0}`, wantPaths: []string{"metadata.labels"}},
		{name: "BMC pair is selected together", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_username", "bmc_password"}},
		{name: "NVOS pair leaves BMC unchanged", body: `{"nvOsUsername":"admin","nvOsPassword":"secret"}`, wantPaths: []string{"nvos_username", "nvos_password"}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var request APIExpectedSwitchUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(test.body), &request))
			require.NoError(t, request.Validate())
			patch := request.ToProto(&cdbm.ExpectedSwitch{SlotID: cutil.GetPtr(int32(0))})
			encoded, err := protojson.Marshal(patch)
			require.NoError(t, err)
			var decoded corev1.PatchExpectedSwitchRequest
			require.NoError(t, protojson.Unmarshal(encoded, &decoded))
			require.NotNil(t, decoded.UpdateMask)
			assert.ElementsMatch(t, test.wantPaths, decoded.GetUpdateMask().GetPaths())
			if request.SlotID != nil {
				labels := decoded.GetExpectedSwitch().GetMetadata().GetLabels()
				require.Len(t, labels, 1)
				assert.Equal(t, "slot_id", labels[0].GetKey())
				assert.Equal(t, "0", labels[0].GetValue())
			}
			if request.DefaultBmcPassword != nil {
				assert.Equal(t, *request.DefaultBmcUsername, decoded.GetExpectedSwitch().GetBmcUsername())
				assert.Equal(t, *request.DefaultBmcPassword, decoded.GetExpectedSwitch().GetBmcPassword())
			}
			if request.NvOsPassword != nil {
				assert.Equal(t, *request.NvOsUsername, decoded.GetExpectedSwitch().GetNvosUsername())
				assert.Equal(t, *request.NvOsPassword, decoded.GetExpectedSwitch().GetNvosPassword())
			}
		})
	}
}
