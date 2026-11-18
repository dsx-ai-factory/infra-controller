// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

// ExpectedInterfaceRole names the interface's Role setting.
type ExpectedInterfaceRole string

// Supported ExpectedInterfaceRole values.
const (
	ExpectedInterfaceRoleUnspecified ExpectedInterfaceRole = "Unspecified"
	ExpectedInterfaceRoleHost        ExpectedInterfaceRole = "Host"
	ExpectedInterfaceRoleDpuOs       ExpectedInterfaceRole = "DpuOs"
	ExpectedInterfaceRoleDpuBmc      ExpectedInterfaceRole = "DpuBmc"
	ExpectedInterfaceRoleHostBmc     ExpectedInterfaceRole = "HostBmc"
)

// ToProto maps a validated setting to its Core enum.
func (v ExpectedInterfaceRole) ToProto() corev1.ExpectedInterfaceRole {
	switch v {
	case ExpectedInterfaceRoleUnspecified:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_UNSPECIFIED
	case ExpectedInterfaceRoleHost:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST
	case ExpectedInterfaceRoleDpuOs:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_OS
	case ExpectedInterfaceRoleDpuBmc:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_BMC
	case ExpectedInterfaceRoleHostBmc:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST_BMC
	default:
		return corev1.ExpectedInterfaceRole(-1)
	}
}

// FromProto maps Core's setting, leaving unknown values empty for validation.
func (v *ExpectedInterfaceRole) FromProto(p corev1.ExpectedInterfaceRole) {
	switch p {
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_UNSPECIFIED:
		*v = ExpectedInterfaceRoleUnspecified
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST:
		*v = ExpectedInterfaceRoleHost
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_OS:
		*v = ExpectedInterfaceRoleDpuOs
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_BMC:
		*v = ExpectedInterfaceRoleDpuBmc
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST_BMC:
		*v = ExpectedInterfaceRoleHostBmc
	default:
		*v = ""
	}
}

// ExpectedInterfaceIPAllocation names the interface's IPAllocation setting.
type ExpectedInterfaceIPAllocation string

// Supported ExpectedInterfaceIPAllocation values.
const (
	ExpectedInterfaceIPAllocationUnspecified ExpectedInterfaceIPAllocation = "Unspecified"
	ExpectedInterfaceIPAllocationDynamic     ExpectedInterfaceIPAllocation = "Dynamic"
	ExpectedInterfaceIPAllocationFixed       ExpectedInterfaceIPAllocation = "Fixed"
	ExpectedInterfaceIPAllocationRetained    ExpectedInterfaceIPAllocation = "Retained"
)

// ToProto maps a validated setting to its Core enum.
func (v ExpectedInterfaceIPAllocation) ToProto() corev1.ExpectedInterfaceIpAllocation {
	switch v {
	case ExpectedInterfaceIPAllocationUnspecified:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_UNSPECIFIED
	case ExpectedInterfaceIPAllocationDynamic:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_DYNAMIC
	case ExpectedInterfaceIPAllocationFixed:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED
	case ExpectedInterfaceIPAllocationRetained:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_RETAINED
	default:
		return corev1.ExpectedInterfaceIpAllocation(-1)
	}
}

// FromProto maps Core's setting, leaving unknown values empty for validation.
func (v *ExpectedInterfaceIPAllocation) FromProto(p corev1.ExpectedInterfaceIpAllocation) {
	switch p {
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_UNSPECIFIED:
		*v = ExpectedInterfaceIPAllocationUnspecified
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_DYNAMIC:
		*v = ExpectedInterfaceIPAllocationDynamic
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED:
		*v = ExpectedInterfaceIPAllocationFixed
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_RETAINED:
		*v = ExpectedInterfaceIPAllocationRetained
	default:
		*v = ""
	}
}

// ExpectedInterfaceNetworkSegmentType names the interface's NetworkSegmentType setting.
type ExpectedInterfaceNetworkSegmentType string

// Supported ExpectedInterfaceNetworkSegmentType values.
const (
	ExpectedInterfaceNetworkSegmentTypeTenant     ExpectedInterfaceNetworkSegmentType = "Tenant"
	ExpectedInterfaceNetworkSegmentTypeAdmin      ExpectedInterfaceNetworkSegmentType = "Admin"
	ExpectedInterfaceNetworkSegmentTypeUnderlay   ExpectedInterfaceNetworkSegmentType = "Underlay"
	ExpectedInterfaceNetworkSegmentTypeHostInband ExpectedInterfaceNetworkSegmentType = "HostInband"
)

// ToProto maps a validated setting to its Core enum.
func (v ExpectedInterfaceNetworkSegmentType) ToProto() corev1.NetworkSegmentType {
	switch v {
	case ExpectedInterfaceNetworkSegmentTypeTenant:
		return corev1.NetworkSegmentType_TENANT
	case ExpectedInterfaceNetworkSegmentTypeAdmin:
		return corev1.NetworkSegmentType_ADMIN
	case ExpectedInterfaceNetworkSegmentTypeUnderlay:
		return corev1.NetworkSegmentType_UNDERLAY
	case ExpectedInterfaceNetworkSegmentTypeHostInband:
		return corev1.NetworkSegmentType_HOST_INBAND
	default:
		return corev1.NetworkSegmentType(-1)
	}
}

// FromProto maps Core's setting, leaving unknown values empty for validation.
func (v *ExpectedInterfaceNetworkSegmentType) FromProto(p corev1.NetworkSegmentType) {
	switch p {
	case corev1.NetworkSegmentType_TENANT:
		*v = ExpectedInterfaceNetworkSegmentTypeTenant
	case corev1.NetworkSegmentType_ADMIN:
		*v = ExpectedInterfaceNetworkSegmentTypeAdmin
	case corev1.NetworkSegmentType_UNDERLAY:
		*v = ExpectedInterfaceNetworkSegmentTypeUnderlay
	case corev1.NetworkSegmentType_HOST_INBAND:
		*v = ExpectedInterfaceNetworkSegmentTypeHostInband
	default:
		*v = ""
	}
}
