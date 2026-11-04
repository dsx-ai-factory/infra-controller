// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"google.golang.org/protobuf/proto"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPITrayDecommissionRequest_Validate(t *testing.T) {
	cases := []struct {
		name, siteID, componentID, trayType string
		valid                               bool
	}{
		{"compute", "11111111-1111-4111-8111-111111111111", "component-id", "Compute", true},
		{"switch", "11111111-1111-4111-8111-111111111111", "component-id", "NVSwitch", true},
		{"power shelf", "11111111-1111-4111-8111-111111111111", "component-id", "PowerShelf", true},
		{"missing site", "", "component-id", "NVSwitch", false},
		{"invalid site", "invalid", "component-id", "NVSwitch", false},
		{"missing component", "11111111-1111-4111-8111-111111111111", "", "NVSwitch", false},
		{"missing type", "11111111-1111-4111-8111-111111111111", "component-id", "", false},
		{"unsupported type", "11111111-1111-4111-8111-111111111111", "component-id", "Switch", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := APITrayDecommissionRequest{SiteID: tc.siteID, TrayID: tc.componentID, Type: tc.trayType}
			if tc.valid {
				assert.NoError(t, request.Validate())
			} else {
				assert.Error(t, request.Validate())
			}
		})
	}
}

func TestAPITrayDecommissionRequest_ToProto(t *testing.T) {
	cases := []struct {
		trayType string
		expected proto.Message
	}{
		{"Compute", &corev1.DecommissionManagedHostRequest{MachineId: &corev1.MachineId{Id: "component-id"}}},
		{"NVSwitch", &corev1.DecommissionSwitchRequest{SwitchId: &corev1.SwitchId{Id: "component-id"}}},
		{"PowerShelf", &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: "component-id"}}},
	}
	for _, tc := range cases {
		t.Run(tc.trayType, func(t *testing.T) {
			request := APITrayDecommissionRequest{Type: tc.trayType, TrayID: "component-id"}
			assert.True(t, proto.Equal(tc.expected, request.ToProto()))
		})
	}
}
