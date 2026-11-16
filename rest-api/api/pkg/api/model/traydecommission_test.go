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
		name, siteID, componentID string
		valid                     bool
	}{
		{"compute", "11111111-1111-4111-8111-111111111111", "fm100-host", true},
		{"switch", "11111111-1111-4111-8111-111111111111", "sw100-switch", true},
		{"power shelf", "11111111-1111-4111-8111-111111111111", "ps100-shelf", true},
		{"missing site", "", "sw100-switch", false},
		{"invalid site", "invalid", "sw100-switch", false},
		{"missing component", "11111111-1111-4111-8111-111111111111", "", false},
		{"unsupported namespace", "11111111-1111-4111-8111-111111111111", "component-id", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := APITrayDecommissionRequest{SiteID: tc.siteID, TrayID: tc.componentID}
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
		componentID string
		expected    proto.Message
	}{
		{"fm100-host", &corev1.DecommissionManagedHostRequest{MachineId: &corev1.MachineId{Id: "fm100-host"}}},
		{"sw100-switch", &corev1.DecommissionSwitchRequest{SwitchId: &corev1.SwitchId{Id: "sw100-switch"}}},
		{"ps100-shelf", &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: "ps100-shelf"}}},
	}
	for _, tc := range cases {
		t.Run(tc.componentID, func(t *testing.T) {
			request := APITrayDecommissionRequest{TrayID: tc.componentID}
			assert.True(t, proto.Equal(tc.expected, request.ToProto()))
		})
	}
}
