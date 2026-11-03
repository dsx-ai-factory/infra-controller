// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestExpectedMachine_ToProto_DpuPolicyAndHostNics(t *testing.T) {
	ignore := ExpectedMachineDpuPolicyIgnore
	segment := expectedHostNicSegmentHostInband
	role := expectedHostNicRoleHost
	allocation := expectedHostNicIPFixed
	fixedIP := "10.0.0.10"
	em := &ExpectedMachine{
		ID:                  uuid.New(),
		BmcMacAddress:       "aa:bb:cc:dd:ee:ff",
		ChassisSerialNumber: "CSN-1",
		DpuPolicy:           &ignore,
		BmcRetainCredentials: cutil.GetPtr(true),
		HostNics: ExpectedHostNics{
			{
				MacAddress:         "11:22:33:44:55:66",
				Primary:            cutil.GetPtr(true),
				NetworkSegmentType: &segment,
				Role:               &role,
				IPAllocation:       &allocation,
				FixedIP:            &fixedIP,
			},
		},
	}

	proto := em.ToProto(ExpectedMachineCredentials{})
	if assert.NotNil(t, proto.DpuMode) {
		assert.Equal(t, corev1.DpuMode_NO_DPU, *proto.DpuMode)
	}
	assert.True(t, proto.GetBmcRetainCredentials())
	if assert.Len(t, proto.HostNics, 1) {
		assert.Equal(t, "11:22:33:44:55:66", proto.HostNics[0].GetMacAddress())
		assert.True(t, proto.HostNics[0].GetPrimary())
		assert.Equal(t, corev1.NetworkSegmentType_HOST_INBAND, proto.HostNics[0].GetNetworkSegmentType())
		assert.Equal(t, corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST, proto.HostNics[0].GetRole())
		assert.Equal(t, corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED, proto.HostNics[0].GetIpAllocation())
		assert.Equal(t, fixedIP, proto.HostNics[0].GetFixedIp())
	}
}

func TestExpectedMachine_FromProto_DpuPolicyAndHostNics(t *testing.T) {
	mode := corev1.DpuMode_NIC_MODE
	segment := corev1.NetworkSegmentType_ADMIN
	role := corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_OS
	allocation := corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_DYNAMIC
	retain := true
	proto := &corev1.ExpectedMachine{
		BmcMacAddress:        "aa:bb:cc:dd:ee:ff",
		ChassisSerialNumber:  "CSN-1",
		DpuMode:              &mode,
		BmcRetainCredentials: &retain,
		HostNics: []*corev1.ExpectedHostNic{
			{
				MacAddress:         cutil.GetPtr("11:22:33:44:55:66"),
				Primary:            cutil.GetPtr(false),
				NetworkSegmentType: &segment,
				Role:               &role,
				IpAllocation:       &allocation,
			},
		},
	}

	em := &ExpectedMachine{}
	em.FromProto(proto, nil)
	if assert.NotNil(t, em.DpuPolicy) {
		assert.Equal(t, ExpectedMachineDpuPolicyNic, *em.DpuPolicy)
	}
	if assert.NotNil(t, em.BmcRetainCredentials) {
		assert.True(t, *em.BmcRetainCredentials)
	}
	if assert.Len(t, em.HostNics, 1) {
		assert.Equal(t, expectedHostNicRoleDpuOS, *em.HostNics[0].Role)
		assert.Equal(t, expectedHostNicIPDynamic, *em.HostNics[0].IPAllocation)
	}
}

func TestExpectedHostNics_ToProto_empty(t *testing.T) {
	assert.Nil(t, ExpectedHostNics{}.ToProto())
}

func TestNetworkSegmentTypeToProto_hostInbandAliases(t *testing.T) {
	for _, in := range []string{"hostInband", "host_inband"} {
		seg, ok := networkSegmentTypeToProto(in)
		assert.True(t, ok)
		assert.Equal(t, corev1.NetworkSegmentType_HOST_INBAND, seg)
	}
}
