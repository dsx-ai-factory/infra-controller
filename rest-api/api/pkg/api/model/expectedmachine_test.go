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
	"google.golang.org/protobuf/proto"
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
			desc: "reject multiple HostBmc declarations",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress: "00:11:22:33:44:55", ChassisSerialNumber: validChassisSerial,
				Interfaces: APIExpectedMachineInterfaces{
					{MacAddress: cutil.GetPtr("00:11:22:33:44:55"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc)},
					{MacAddress: cutil.GetPtr("00:11:22:33:44:55"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc)},
				},
			},
			expectErr: true,
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
			desc: "error when BmcIpAddress is unspecified",
			obj: APIExpectedMachineCreateRequest{
				BmcMacAddress:       "00:11:22:33:44:55",
				ChassisSerialNumber: validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("0.0.0.0"),
			},
			expectErr: true,
		},
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

func TestAPIExpectedMachineInterface_Validate(t *testing.T) {
	tests := []struct {
		name    string
		value   APIExpectedMachineInterface
		wantErr string
	}{
		{
			name: "valid CX9 interface",
			value: APIExpectedMachineInterface{
				MacAddress: cutil.GetPtr("02:00:00:00:00:09"),
				NicType:    cutil.GetPtr("CX9"),
				FixedIP:    cutil.GetPtr("2001:db8::9"),
			},
		},
		{name: "invalid MAC", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("invalid")}, wantErr: "macAddress"},
		{name: "non-six-octet MAC", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("01:23:45:67:89:ab:cd:ef")}, wantErr: "six colon- or hyphen-separated octets"},
		{name: "invalid fixed IP", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), FixedIP: cutil.GetPtr("invalid")}, wantErr: "FixedIP"},
		{name: "unspecified fixed IP", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), FixedIP: cutil.GetPtr("0.0.0.0")}, wantErr: "not unspecified, multicast, or limited broadcast"},
		{name: "whitespace NIC type", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), NicType: cutil.GetPtr(" ")}, wantErr: "NicType"},
		{name: "invalid gateway", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), FixedGateway: cutil.GetPtr("invalid")}, wantErr: "fixedGateway"},
		{name: "unknown role", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRole("invalid"))}, wantErr: "role"},
		{name: "empty segment", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), NetworkSegmentType: cutil.GetPtr(cdbm.ExpectedInterfaceNetworkSegmentType(""))}, wantErr: "networkSegmentType"},
		{name: "unknown allocation", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), IPAllocation: cutil.GetPtr(cdbm.ExpectedInterfaceIPAllocation("invalid"))}, wantErr: "ipAllocation"},
		{name: "Fixed requires address", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), IPAllocation: cutil.GetPtr(cdbm.ExpectedInterfaceIPAllocationFixed)}, wantErr: "fixedIp"},
		{name: "Dynamic forbids address", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), IPAllocation: cutil.GetPtr(cdbm.ExpectedInterfaceIPAllocationDynamic), FixedIP: cutil.GetPtr("192.0.2.9")}, wantErr: "fixedIp"},
		{name: "DPU forbids primary", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleDpuOs), Primary: cutil.GetPtr(false)}, wantErr: "primary"},
		{name: "HostBmc forbids primary true", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc), Primary: cutil.GetPtr(true)}, wantErr: "primary"},
		{name: "HostBmc accepts primary false", value: APIExpectedMachineInterface{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc), Primary: cutil.GetPtr(false)}},
		{name: "identifier without deprecated MAC", value: APIExpectedMachineInterface{Identifier: macIdentifier("02:00:00:00:00:09")}},
		{name: "no identity", value: APIExpectedMachineInterface{NicType: cutil.GetPtr("CX9")}, wantErr: "identifier: a value is required"},
		{name: "invalid identifier", value: APIExpectedMachineInterface{Identifier: &APIExpectedMachineInterfaceIdentifier{}}, wantErr: "exactly one of macAddress or astra"},
		{name: "deprecated MAC matches identifier in another spelling", value: APIExpectedMachineInterface{
			Identifier: macIdentifier("02:aa:bb:cc:dd:ee"), MacAddress: cutil.GetPtr("02-AA-BB-CC-DD-EE"),
		}},
		{name: "deprecated MAC differs from identifier", value: APIExpectedMachineInterface{
			Identifier: macIdentifier("02:aa:bb:cc:dd:ee"), MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ef"),
		}, wantErr: "must match identifier.macAddress"},
		{name: "deprecated MAC beside Astra identifier", value: APIExpectedMachineInterface{
			Identifier: astraIdentifier("C1-1-L1", nil), MacAddress: cutil.GetPtr("02:00:00:00:00:09"),
		}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.value.Validate()
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func macIdentifier(mac string) *APIExpectedMachineInterfaceIdentifier {
	return &APIExpectedMachineInterfaceIdentifier{MacAddress: cutil.GetPtr(APIExpectedMachineInterfaceMacAddressIdentifier(mac))}
}

func astraIdentifier(physicalName string, logicalName *string) *APIExpectedMachineInterfaceIdentifier {
	return &APIExpectedMachineInterfaceIdentifier{Astra: &APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: physicalName, LogicalName: logicalName}}
}

func TestAPIExpectedMachineInterfaceIdentifier_Validate(t *testing.T) {
	for _, tc := range []struct {
		name       string
		identifier *APIExpectedMachineInterfaceIdentifier
		wantErr    string
	}{
		{name: "MAC address", identifier: macIdentifier("02:00:00:00:00:09")},
		{name: "Astra names", identifier: astraIdentifier("C1-1-L1", cutil.GetPtr("eth_r0_p0"))},
		{name: "no identity", identifier: &APIExpectedMachineInterfaceIdentifier{}, wantErr: "exactly one of macAddress or astra"},
		{name: "both identities", identifier: &APIExpectedMachineInterfaceIdentifier{
			MacAddress: macIdentifier("02:00:00:00:00:09").MacAddress, Astra: astraIdentifier("C1-1-L1", nil).Astra,
		}, wantErr: "exactly one of macAddress or astra"},
		{name: "empty MAC", identifier: macIdentifier(""), wantErr: "macAddress: MacAddress cannot be empty"},
		{name: "malformed MAC", identifier: macIdentifier("02:00:00:00:00"), wantErr: "macAddress: must be a MAC address of six colon- or hyphen-separated octets"},
		{name: "invalid Astra names", identifier: astraIdentifier("", nil), wantErr: "astra: (physicalName: a value is required.)"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.identifier.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

func TestAPIExpectedMachineInterfaceAstraIdentifier_Validate(t *testing.T) {
	for _, tc := range []struct {
		name    string
		astra   APIExpectedMachineInterfaceAstraIdentifier
		wantErr string
	}{
		{name: "physical and logical names", astra: APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1-1-L1", LogicalName: cutil.GetPtr("eth_r0_p0")}},
		{name: "logical name is optional", astra: APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1-1-L1"}},
		{name: "empty physical name", astra: APIExpectedMachineInterfaceAstraIdentifier{}, wantErr: "physicalName: a value is required"},
		{name: "physical name outside Core's characters", astra: APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1 1 L1"}, wantErr: "physicalName: may contain only"},
		{name: "empty logical name", astra: APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1-1-L1", LogicalName: cutil.GetPtr("")}, wantErr: "logicalName: LogicalName cannot be empty"},
		{name: "logical name rejects the colon physical names allow", astra: APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1-1-L1", LogicalName: cutil.GetPtr("eth:0")}, wantErr: "logicalName: may contain only"},
		{name: "logical name over 127 characters", astra: APIExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1-1-L1", LogicalName: cutil.GetPtr(strings.Repeat("a", 128))}, wantErr: "logicalName: must be at most 127 characters"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.astra.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

func TestAPIExpectedMachineInterfaces_Validate(t *testing.T) {
	for _, tc := range []struct {
		name       string
		interfaces APIExpectedMachineInterfaces
		wantErr    string
	}{
		{name: "one HostBmc and one primary", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:00:00:00:00:01"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc)},
			{MacAddress: cutil.GetPtr("02:00:00:00:00:02"), Primary: cutil.GetPtr(true)},
			{MacAddress: cutil.GetPtr("02:00:00:00:00:03"), Primary: cutil.GetPtr(false)},
		}},
		{name: "duplicate HostBmc", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:00:00:00:00:01"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc)},
			{MacAddress: cutil.GetPtr("02:00:00:00:00:01"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHostBmc)},
		}, wantErr: "at most one HostBmc"},
		{name: "multiple primaries including omitted role", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:00:00:00:00:01"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleHost), Primary: cutil.GetPtr(true)},
			{MacAddress: cutil.GetPtr("02:00:00:00:00:02"), Primary: cutil.GetPtr(true)},
		}, wantErr: "at most one interface may set primary"},
		{name: "dual stack same MAC with different spelling", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee"), FixedIP: cutil.GetPtr("192.0.2.9")},
			{MacAddress: cutil.GetPtr("02-AA-BB-CC-DD-EE"), FixedIP: cutil.GetPtr("2001:db8::9")},
		}},
		{name: "invalid entry still rejected", interfaces: APIExpectedMachineInterfaces{{MacAddress: cutil.GetPtr("invalid")}}, wantErr: "macAddress"},
		{name: "duplicate IPv4 with normalized MAC", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee"), FixedIP: cutil.GetPtr("192.0.2.9")},
			{MacAddress: cutil.GetPtr("02-AA-BB-CC-DD-EE"), FixedIP: cutil.GetPtr("192.0.2.10")},
		}},
		{name: "duplicate IPv6", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee"), FixedIP: cutil.GetPtr("2001:db8::9")},
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee"), FixedIP: cutil.GetPtr("2001:db8::10")},
		}},
		{name: "omitted fixed IP has no family", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee")},
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee")},
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee"), FixedIP: cutil.GetPtr("192.0.2.9")},
		}},
		{name: "zero MAC CX9 declarations with distinct IPv4 addresses", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("00:00:00:00:00:00"), NicType: cutil.GetPtr("CX9"), FixedIP: cutil.GetPtr("192.0.2.9")},
			{MacAddress: cutil.GetPtr("00:00:00:00:00:00"), NicType: cutil.GetPtr("CX9"), FixedIP: cutil.GetPtr("192.0.2.10")},
		}},
		{name: "different MACs may declare the same family", interfaces: APIExpectedMachineInterfaces{
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ee"), FixedIP: cutil.GetPtr("192.0.2.9")},
			{MacAddress: cutil.GetPtr("02:aa:bb:cc:dd:ef"), FixedIP: cutil.GetPtr("192.0.2.10")},
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.interfaces.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
			} else {
				require.ErrorContains(t, err, tc.wantErr)
			}
		})
	}
}

func TestAPIExpectedMachineInterface_ToDBModel(t *testing.T) {
	t.Run("canonical MAC spelling", func(t *testing.T) {
		for _, mac := range []string{"02:aa:bb:cc:dd:ee", "02-AA-BB-CC-DD-EE", "02:aa-bb:cc-dd:ee", "02:AA:BB:CC:DD:EE"} {
			t.Run(mac, func(t *testing.T) {
				value := APIExpectedMachineInterface{Identifier: macIdentifier(mac)}
				require.NoError(t, value.Validate())
				stored := value.ToDBModel()
				require.NotNil(t, stored.MacAddress)
				assert.Equal(t, "02:AA:BB:CC:DD:EE", *stored.MacAddress)
				assert.Equal(t, stored.MacAddress, stored.ToProto().MacAddress)
				response := NewAPIExpectedMachineInterface(stored)
				assert.Equal(t, macIdentifier("02:AA:BB:CC:DD:EE"), response.Identifier)
				assert.Equal(t, stored.MacAddress, response.MacAddress)
			})
		}
	})
	for _, tc := range []struct {
		name  string
		input *corev1.ExpectedHostNic
	}{
		{name: "complete declaration", input: &corev1.ExpectedHostNic{
			MacAddress: cutil.GetPtr("02:00:00:00:00:09"), NicType: cutil.GetPtr("CX9"), FixedIp: cutil.GetPtr("192.0.2.9"),
			FixedMask: cutil.GetPtr("255.255.255.0"), FixedGateway: cutil.GetPtr("192.0.2.1"), Primary: cutil.GetPtr(true),
			NetworkSegmentType: corev1.NetworkSegmentType_HOST_INBAND.Enum(),
			Role:               corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST.Enum(),
			IpAllocation:       corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED.Enum(),
		}},
		{name: "legacy declaration without new fields", input: &corev1.ExpectedHostNic{MacAddress: cutil.GetPtr("02:00:00:00:00:09")}},
		{name: "Astra names without a MAC", input: &corev1.ExpectedHostNic{
			PhysicalIfname: cutil.GetPtr("C1-1-L1"), LogicalIfname: cutil.GetPtr("eth_r0_p0"),
			NicType: cutil.GetPtr("CX9"), FixedIp: cutil.GetPtr("192.0.2.11"),
		}},
		{name: "Astra names and MAC round-trip together", input: &corev1.ExpectedHostNic{
			MacAddress:     cutil.GetPtr("02:00:00:00:00:0A"),
			PhysicalIfname: cutil.GetPtr("C1-1-L2"), LogicalIfname: cutil.GetPtr("eth_r0_p1"),
			NicType: cutil.GetPtr("CX9"), FixedIp: cutil.GetPtr("192.0.2.11"),
		}},
		{name: "explicit zero values survive", input: &corev1.ExpectedHostNic{
			MacAddress: cutil.GetPtr("02:00:00:00:00:09"), Primary: cutil.GetPtr(false),
			NetworkSegmentType: corev1.NetworkSegmentType_TENANT.Enum(),
			Role:               corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_UNSPECIFIED.Enum(),
			IpAllocation:       corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_UNSPECIFIED.Enum(),
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var stored cdbm.ExpectedMachineInterface
			stored.FromProto(tc.input)
			persisted, err := json.Marshal(stored)
			require.NoError(t, err)
			var restored cdbm.ExpectedMachineInterface
			require.NoError(t, json.Unmarshal(persisted, &restored))
			response := NewAPIExpectedMachine(&cdbm.ExpectedMachine{Interfaces: []cdbm.ExpectedMachineInterface{restored}})
			body, err := json.Marshal(response)
			require.NoError(t, err)
			var request APIExpectedMachineUpdateRequest
			require.NoError(t, json.Unmarshal(body, &request))
			require.Len(t, request.Interfaces, 1)
			require.NoError(t, request.Interfaces[0].Validate())
			assert.Equal(t, restored, request.Interfaces[0].ToDBModel(), "round-trip must preserve every stored interface attribute")
			patch := request.ToProto(&cdbm.ExpectedMachine{Interfaces: request.InterfacesToDBModel()})
			wire, err := protojson.Marshal(patch)
			require.NoError(t, err)
			var decoded corev1.PatchExpectedMachineRequest
			require.NoError(t, protojson.Unmarshal(wire, &decoded))
			require.Len(t, decoded.ExpectedMachine.HostNics, 1)
			assert.True(t, proto.Equal(tc.input, decoded.ExpectedMachine.HostNics[0]), "%v", decoded.ExpectedMachine.HostNics[0])
		})
	}
}

func TestAPIExpectedMachineUpdateRequest_InterfacesJSONSemantics(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		wantNil    bool
		wantLength int
	}{
		{name: "omitted preserves", body: `{}`, wantNil: true},
		{name: "null preserves", body: `{"interfaces":null}`, wantNil: true},
		{name: "empty clears", body: `{"interfaces":[]}`},
		{name: "populated replaces", body: `{"interfaces":[{"macAddress":"02:00:00:00:00:09","nicType":"CX9","fixedIp":"192.0.2.9"}]}`, wantLength: 1},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var got APIExpectedMachineUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(test.body), &got))
			assert.Equal(t, test.wantNil, got.Interfaces == nil)
			assert.Len(t, got.Interfaces, test.wantLength)
		})
	}
}

func TestNewAPIExpectedMachine(t *testing.T) {
	tests := []struct {
		desc                 string
		isDpfEnabled         *bool
		retainBmcCredentials *bool
		hostLifecycleProfile cdbm.HostLifecycleProfile
		wantDpfJSON          string
		wantRetainJSON       string
		wantProfile          *APIHostLifecycleProfile
		labels               cdbm.Labels
		dpuSerials           []string
		interfaces           []cdbm.ExpectedMachineInterface
		wantLabelsJSON       string
		wantSerialsJSON      string
		wantInterfacesJSON   string
	}{
		{
			desc:               "nil collections serialize as empty and unset DPF defaults to true",
			wantDpfJSON:        "true",
			wantLabelsJSON:     `{}`,
			wantSerialsJSON:    `[]`,
			wantInterfacesJSON: `[]`,
		},
		{
			desc:               "empty collections remain empty",
			labels:             cdbm.Labels{},
			dpuSerials:         []string{},
			wantLabelsJSON:     `{}`,
			wantSerialsJSON:    `[]`,
			wantInterfacesJSON: `[]`,
		},
		{
			desc:               "populated collections preserve values and order",
			labels:             cdbm.Labels{"env": "test", "zone": "us-west-1"},
			dpuSerials:         []string{"DPU002", "DPU001"},
			wantLabelsJSON:     `{"env":"test","zone":"us-west-1"}`,
			wantSerialsJSON:    `["DPU002","DPU001"]`,
			interfaces:         []cdbm.ExpectedMachineInterface{{MacAddress: cutil.GetPtr("02:00:00:00:00:09"), NicType: cutil.GetPtr("CX9"), FixedIP: cutil.GetPtr("192.0.2.9")}},
			wantInterfacesJSON: `[{"identifier":{"macAddress":"02:00:00:00:00:09","astra":null},"macAddress":"02:00:00:00:00:09","nicType":"CX9","fixedIp":"192.0.2.9","fixedMask":null,"fixedGateway":null,"primary":null,"networkSegmentType":null,"role":null,"ipAllocation":null}]`,
		},
		{
			desc:       "Astra interface returns its names as the identifier",
			interfaces: []cdbm.ExpectedMachineInterface{{PhysicalName: cutil.GetPtr("C1-1-L1"), LogicalName: cutil.GetPtr("eth_r0_p0"), NicType: cutil.GetPtr("CX9")}},
			wantInterfacesJSON: `[{"identifier":{"macAddress":null,"astra":{"physicalName":"C1-1-L1","logicalName":"eth_r0_p0"}},"macAddress":null,
				"nicType":"CX9","fixedIp":null,"fixedMask":null,"fixedGateway":null,"primary":null,"networkSegmentType":null,"role":null,"ipAllocation":null}]`,
		},
		{
			desc:       "MAC Core reports beside Astra names stays in the deprecated field",
			interfaces: []cdbm.ExpectedMachineInterface{{MacAddress: cutil.GetPtr("02:00:00:00:00:0A"), PhysicalName: cutil.GetPtr("C1-1-L2")}},
			wantInterfacesJSON: `[{"identifier":{"macAddress":null,"astra":{"physicalName":"C1-1-L2","logicalName":null}},"macAddress":"02:00:00:00:00:0A",
				"nicType":null,"fixedIp":null,"fixedMask":null,"fixedGateway":null,"primary":null,"networkSegmentType":null,"role":null,"ipAllocation":null}]`,
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
			desc:           "unset BMC credential retention is returned as false",
			wantRetainJSON: "false",
		},
		{
			desc:                 "stored BMC credential retention true is returned",
			retainBmcCredentials: cutil.GetPtr(true),
			wantRetainJSON:       "true",
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
				RetainBmcCredentials:     tc.retainBmcCredentials,
				HostLifecycleProfile:     tc.hostLifecycleProfile,
				Interfaces:               tc.interfaces,
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
			if tc.wantRetainJSON != "" {
				assert.Equal(t, tc.wantRetainJSON, string(fields["retainBmcCredentials"]))
			}
			if tc.wantInterfacesJSON != "" {
				assert.JSONEq(t, tc.wantInterfacesJSON, string(fields["interfaces"]))
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
			desc: "reject multiple primary interfaces",
			obj: APIExpectedMachineUpdateRequest{Interfaces: APIExpectedMachineInterfaces{
				{MacAddress: cutil.GetPtr("02:00:00:00:00:01"), Primary: cutil.GetPtr(true)},
				{MacAddress: cutil.GetPtr("02:00:00:00:00:02"), Primary: cutil.GetPtr(true)},
			}},
			expectErr: true,
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
			desc: "error when BmcIpAddress is limited broadcast",
			obj: APIExpectedMachineUpdateRequest{
				ChassisSerialNumber: &validChassisSerial,
				BmcIpAddress:        cutil.GetPtr("255.255.255.255"),
			},
			expectErr: true,
		},
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
		{name: "null fields preserve Core state", body: `{"defaultBmcUsername":null,"defaultBmcPassword":null,"labels":null,"slotId":null,"bmcIpAddress":null,"retainBmcCredentials":null}`},
		{name: "explicit zero and empty values remain selected", body: `{"slotId":0,"labels":{},"fallbackDPUSerialNumbers":[],"isDpfEnabled":false,"retainBmcCredentials":false,"hostLifecycleProfile":{"disableLockdown":false},"bmcIpAddress":""}`, wantPaths: []string{"bmc_ip_address", "metadata.labels", "fallback_dpu_serial_numbers", "is_dpf_enabled", "bmc_retain_credentials", "host_lifecycle_profile.disable_lockdown"}},
		{name: "BMC address selects automatic allocation", body: `{"bmcIpAddress":"192.0.2.31"}`, wantPaths: []string{"bmc_ip_address", "bmc_ip_allocation"}},
		{name: "slot ID alone selects derived labels", body: `{"slotId":0}`, wantPaths: []string{"metadata.labels"}},
		{name: "BMC username leaves the password unselected", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":null}`, wantPaths: []string{"bmc_username"}},
		{name: "BMC password leaves the username unselected", body: `{"defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_password"}},
		{name: "BMC pair is selected together", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_username", "bmc_password"}},
		{name: "empty lifecycle profile preserves policy", body: `{"hostLifecycleProfile":{}}`},
		{name: "interfaces replace Core host NICs", body: `{"interfaces":[{"macAddress":"02:00:00:00:00:09","nicType":"CX9","fixedIp":"192.0.2.9"}]}`, wantPaths: []string{"host_nics"}},
		{name: "empty interfaces clear Core host NICs", body: `{"interfaces":[]}`, wantPaths: []string{"host_nics"}},
		{name: "server name selects server_name path", body: `{"serverName":"server-1"}`, wantPaths: []string{"server_name"}},
		{name: "empty server name clears Core state", body: `{"serverName":""}`, wantPaths: []string{"server_name"}},
		{name: "null server name preserves Core state", body: `{"serverName":null}`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var request APIExpectedMachineUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(test.body), &request))
			require.NoError(t, request.Validate())
			// The stored retention value differs from the explicit false request
			// so the patch must carry the request value, not the row's.
			patch := request.ToProto(&cdbm.ExpectedMachine{SlotID: cutil.GetPtr(int32(0)), IsDpfEnabled: cutil.GetPtr(false), RetainBmcCredentials: cutil.GetPtr(true), ServerName: cutil.GetPtr("server-1"), HostLifecycleProfile: cdbm.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)}})
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
			if request.RetainBmcCredentials != nil {
				assert.Equal(t, request.RetainBmcCredentials, decoded.GetExpectedMachine().BmcRetainCredentials)
			}
			if request.BmcIpAddress != nil && *request.BmcIpAddress != "" {
				assert.Equal(t, corev1.BmcIpAllocationType_BMC_IP_ALLOCATION_TYPE_AUTO, decoded.GetExpectedMachine().GetBmcIpAllocation())
			}
			if request.ServerName != nil {
				assert.Equal(t, cutil.GetPtrIfNotZero(*request.ServerName), decoded.GetExpectedMachine().ServerName)
			}
			if request.Interfaces != nil {
				require.Len(t, decoded.GetExpectedMachine().GetHostNics(), len(request.Interfaces))
				if len(request.Interfaces) > 0 {
					assert.Equal(t, "02:00:00:00:00:09", decoded.GetExpectedMachine().GetHostNics()[0].GetMacAddress())
					assert.Equal(t, "CX9", decoded.GetExpectedMachine().GetHostNics()[0].GetNicType())
					assert.Equal(t, "192.0.2.9", decoded.GetExpectedMachine().GetHostNics()[0].GetFixedIp())
				}
			}
		})
	}
}
