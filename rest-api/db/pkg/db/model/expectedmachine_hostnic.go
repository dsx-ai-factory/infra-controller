// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strings"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	ExpectedMachineDpuPolicyManage = "manage"
	ExpectedMachineDpuPolicyNic    = "nic"
	ExpectedMachineDpuPolicyIgnore = "ignore"

	expectedHostNicSegmentTenant     = "tenant"
	expectedHostNicSegmentAdmin      = "admin"
	expectedHostNicSegmentUnderlay   = "underlay"
	expectedHostNicSegmentHostInband = "hostInband"

	expectedHostNicRoleHost    = "host"
	expectedHostNicRoleDpuOS   = "dpuOs"
	expectedHostNicRoleDpuBMC  = "dpuBmc"
	expectedHostNicRoleHostBMC = "hostBmc"

	expectedHostNicIPDynamic  = "dynamic"
	expectedHostNicIPFixed    = "fixed"
	expectedHostNicIPRetained = "retained"
)

// ExpectedHostNic is one declared host interface on an ExpectedMachine.
type ExpectedHostNic struct {
	MacAddress         string  `json:"mac_address"`
	Primary            *bool   `json:"primary,omitempty"`
	NetworkSegmentType *string `json:"network_segment_type,omitempty"`
	NicType            *string `json:"nic_type,omitempty"`
	FixedIP            *string `json:"fixed_ip,omitempty"`
	FixedMask          *string `json:"fixed_mask,omitempty"`
	FixedGateway       *string `json:"fixed_gateway,omitempty"`
	Role               *string `json:"role,omitempty"`
	IPAllocation       *string `json:"ip_allocation,omitempty"`
}

// ExpectedHostNics is the persisted list of host interfaces for an ExpectedMachine.
type ExpectedHostNics []ExpectedHostNic

func normalizeNetworkSegmentType(segmentType string) string {
	switch segmentType {
	case "host_inband", "hostInband", "HOST_INBAND":
		return expectedHostNicSegmentHostInband
	case expectedHostNicSegmentTenant, expectedHostNicSegmentAdmin, expectedHostNicSegmentUnderlay:
		return segmentType
	default:
		return segmentType
	}
}

func dpuPolicyToProto(policy string) (corev1.DpuMode, bool) {
	switch strings.ToLower(policy) {
	case ExpectedMachineDpuPolicyManage:
		return corev1.DpuMode_DPU_MODE, true
	case ExpectedMachineDpuPolicyNic:
		return corev1.DpuMode_NIC_MODE, true
	case ExpectedMachineDpuPolicyIgnore:
		return corev1.DpuMode_NO_DPU, true
	default:
		return corev1.DpuMode_DPU_MODE_UNSPECIFIED, false
	}
}

func dpuModeFromProto(mode corev1.DpuMode) *string {
	switch mode {
	case corev1.DpuMode_DPU_MODE:
		return ptrString(ExpectedMachineDpuPolicyManage)
	case corev1.DpuMode_NIC_MODE:
		return ptrString(ExpectedMachineDpuPolicyNic)
	case corev1.DpuMode_NO_DPU:
		return ptrString(ExpectedMachineDpuPolicyIgnore)
	default:
		return nil
	}
}

func networkSegmentTypeToProto(segmentType string) (corev1.NetworkSegmentType, bool) {
	switch normalizeNetworkSegmentType(segmentType) {
	case expectedHostNicSegmentTenant:
		return corev1.NetworkSegmentType_TENANT, true
	case expectedHostNicSegmentAdmin:
		return corev1.NetworkSegmentType_ADMIN, true
	case expectedHostNicSegmentUnderlay:
		return corev1.NetworkSegmentType_UNDERLAY, true
	case expectedHostNicSegmentHostInband:
		return corev1.NetworkSegmentType_HOST_INBAND, true
	default:
		return corev1.NetworkSegmentType_TENANT, false
	}
}

func networkSegmentTypeFromProto(segmentType corev1.NetworkSegmentType) *string {
	switch segmentType {
	case corev1.NetworkSegmentType_TENANT:
		return ptrString(expectedHostNicSegmentTenant)
	case corev1.NetworkSegmentType_ADMIN:
		return ptrString(expectedHostNicSegmentAdmin)
	case corev1.NetworkSegmentType_UNDERLAY:
		return ptrString(expectedHostNicSegmentUnderlay)
	case corev1.NetworkSegmentType_HOST_INBAND:
		return ptrString(expectedHostNicSegmentHostInband)
	default:
		return nil
	}
}

func interfaceRoleToProto(role string) (corev1.ExpectedInterfaceRole, bool) {
	switch role {
	case expectedHostNicRoleHost:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST, true
	case expectedHostNicRoleDpuOS, "dpu_os":
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_OS, true
	case expectedHostNicRoleDpuBMC, "dpu_bmc":
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_BMC, true
	case expectedHostNicRoleHostBMC, "host_bmc":
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST_BMC, true
	default:
		return corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_UNSPECIFIED, false
	}
}

func interfaceRoleFromProto(role corev1.ExpectedInterfaceRole) *string {
	switch role {
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST:
		return ptrString(expectedHostNicRoleHost)
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_OS:
		return ptrString(expectedHostNicRoleDpuOS)
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_DPU_BMC:
		return ptrString(expectedHostNicRoleDpuBMC)
	case corev1.ExpectedInterfaceRole_EXPECTED_INTERFACE_ROLE_HOST_BMC:
		return ptrString(expectedHostNicRoleHostBMC)
	default:
		return nil
	}
}

func ipAllocationToProto(allocation string) (corev1.ExpectedInterfaceIpAllocation, bool) {
	switch allocation {
	case expectedHostNicIPDynamic:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_DYNAMIC, true
	case expectedHostNicIPFixed:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED, true
	case expectedHostNicIPRetained:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_RETAINED, true
	default:
		return corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_UNSPECIFIED, false
	}
}

func ipAllocationFromProto(allocation corev1.ExpectedInterfaceIpAllocation) *string {
	switch allocation {
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_DYNAMIC:
		return ptrString(expectedHostNicIPDynamic)
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_FIXED:
		return ptrString(expectedHostNicIPFixed)
	case corev1.ExpectedInterfaceIpAllocation_EXPECTED_INTERFACE_IP_ALLOCATION_RETAINED:
		return ptrString(expectedHostNicIPRetained)
	default:
		return nil
	}
}

func ptrString(v string) *string {
	return &v
}

func expectedHostNicToProto(nic ExpectedHostNic) *corev1.ExpectedHostNic {
	proto := &corev1.ExpectedHostNic{
		Primary: nic.Primary,
	}
	if nic.MacAddress != "" {
		proto.MacAddress = ptrString(nic.MacAddress)
	}
	if nic.NicType != nil && *nic.NicType != "" {
		proto.NicType = nic.NicType
	}
	if nic.FixedIP != nil && *nic.FixedIP != "" {
		proto.FixedIp = nic.FixedIP
	}
	if nic.FixedMask != nil && *nic.FixedMask != "" {
		proto.FixedMask = nic.FixedMask
	}
	if nic.FixedGateway != nil && *nic.FixedGateway != "" {
		proto.FixedGateway = nic.FixedGateway
	}
	if nic.NetworkSegmentType != nil && *nic.NetworkSegmentType != "" {
		if segmentType, ok := networkSegmentTypeToProto(*nic.NetworkSegmentType); ok {
			proto.NetworkSegmentType = &segmentType
		}
	}
	if nic.Role != nil && *nic.Role != "" {
		if role, ok := interfaceRoleToProto(*nic.Role); ok {
			proto.Role = &role
		}
	}
	if nic.IPAllocation != nil && *nic.IPAllocation != "" {
		if allocation, ok := ipAllocationToProto(*nic.IPAllocation); ok {
			proto.IpAllocation = &allocation
		}
	}
	return proto
}

// ToProto converts declared host NICs to workflow protos.
func (nics ExpectedHostNics) ToProto() []*corev1.ExpectedHostNic {
	if len(nics) == 0 {
		return nil
	}
	out := make([]*corev1.ExpectedHostNic, 0, len(nics))
	for _, nic := range nics {
		out = append(out, expectedHostNicToProto(nic))
	}
	return out
}

// FromProto replaces the receiver from workflow host NIC protos.
func (nics *ExpectedHostNics) FromProto(protos []*corev1.ExpectedHostNic) {
	if len(protos) == 0 {
		*nics = ExpectedHostNics{}
		return
	}
	out := make(ExpectedHostNics, 0, len(protos))
	for _, proto := range protos {
		if proto == nil {
			continue
		}
		nic := ExpectedHostNic{
			MacAddress:   proto.GetMacAddress(),
			Primary:      proto.Primary,
			NicType:      proto.NicType,
			FixedIP:      proto.FixedIp,
			FixedMask:    proto.FixedMask,
			FixedGateway: proto.FixedGateway,
		}
		if proto.NetworkSegmentType != nil {
			nic.NetworkSegmentType = networkSegmentTypeFromProto(proto.GetNetworkSegmentType())
		}
		if proto.Role != nil {
			nic.Role = interfaceRoleFromProto(proto.GetRole())
		}
		if proto.IpAllocation != nil {
			nic.IPAllocation = ipAllocationFromProto(proto.GetIpAllocation())
		}
		out = append(out, nic)
	}
	*nics = out
}
