// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"testing"
)

func TestExpectedInterfaceRole_ToProto(t *testing.T) {
	for _, tc := range []struct {
		value ExpectedInterfaceRole
		wire  corev1.ExpectedInterfaceRole
	}{
		{ExpectedInterfaceRoleUnspecified, corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_UNSPECIFIED},
		{ExpectedInterfaceRoleHost, corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST},
		{ExpectedInterfaceRoleDpuOs, corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_OS},
		{ExpectedInterfaceRoleDpuBmc, corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_BMC},
		{ExpectedInterfaceRoleHostBmc, corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST_BMC},
	} {
		t.Run(string(tc.value), func(t *testing.T) {
			assert.Equal(t, tc.wire, tc.value.ToProto())
			var restored ExpectedInterfaceRole
			restored.FromProto(tc.wire)
			assert.Equal(t, tc.value, restored)
		})
	}
}

func TestExpectedInterfaceIPAllocation_ToProto(t *testing.T) {
	for _, tc := range []struct {
		value ExpectedInterfaceIPAllocation
		wire  corev1.ExpectedInterfaceIpAllocation
	}{
		{ExpectedInterfaceIPAllocationUnspecified, corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_UNSPECIFIED},
		{ExpectedInterfaceIPAllocationDynamic, corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_DYNAMIC},
		{ExpectedInterfaceIPAllocationFixed, corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED},
		{ExpectedInterfaceIPAllocationRetained, corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_RETAINED},
	} {
		t.Run(string(tc.value), func(t *testing.T) {
			assert.Equal(t, tc.wire, tc.value.ToProto())
			var restored ExpectedInterfaceIPAllocation
			restored.FromProto(tc.wire)
			assert.Equal(t, tc.value, restored)
		})
	}
}

func TestExpectedInterfaceNetworkSegmentType_ToProto(t *testing.T) {
	for _, tc := range []struct {
		value ExpectedInterfaceNetworkSegmentType
		wire  corev1.NetworkSegmentType
	}{
		{ExpectedInterfaceNetworkSegmentTypeTenant, corev1.NetworkSegmentType_TENANT},
		{ExpectedInterfaceNetworkSegmentTypeAdmin, corev1.NetworkSegmentType_ADMIN},
		{ExpectedInterfaceNetworkSegmentTypeUnderlay, corev1.NetworkSegmentType_UNDERLAY},
		{ExpectedInterfaceNetworkSegmentTypeHostInband, corev1.NetworkSegmentType_HOST_INBAND},
	} {
		t.Run(string(tc.value), func(t *testing.T) {
			assert.Equal(t, tc.wire, tc.value.ToProto())
			var restored ExpectedInterfaceNetworkSegmentType
			restored.FromProto(tc.wire)
			assert.Equal(t, tc.value, restored)
		})
	}
}
