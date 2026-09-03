// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

import (
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
)

// ManagerHdl - local handle to be assigned
var ManagerHandler ManagerAPI

// ManagerAccess - access to all APIs/data/conf
// nolint
type ManagerAccess struct {
	API  *ManagerAPI
	Data *ManagerData
	Conf *ManagerConf
}

// ManagerData - super struct
type ManagerData struct {
	EB *elektratypes.Elektra
}

// ManagerAPI struct to hold all mgr interface
type ManagerAPI struct {
	// Add all the manager interfaces here
	Bootstrap              BootstrapInterface
	Site                   SiteInterface
	VPC                    VPCInterface
	VpcPrefix              VpcPrefixInterface
	VpcPeering             VpcPeeringInterface
	SpectrumXPartition     SpectrumXPartitionInterface
	Subnet                 SubnetInterface
	Instance               InstanceInterface
	Machine                MachineInterface
	Orchestrator           OrchestratorInterface
	CoreGrpc               CoreGrpcInterface
	SSHKeyGroup            SSHKeyGroupInterface
	InfiniBandPartition    InfiniBandPartitionInterface
	Tenant                 TenantInterface
	OperatingSystem        OperatingSystemInterface
	MachineValidation      MachineValidationInterface
	InstanceType           InstanceTypeInterface
	NetworkSecurityGroup   NetworkSecurityGroupInterface
	ExpectedMachine        ExpectedMachineInterface
	ExpectedPowerShelf     ExpectedPowerShelfInterface
	ExpectedRack           ExpectedRackInterface
	ExpectedSwitch         ExpectedSwitchInterface
	SKU                    SKUInterface
	DpuExtensionService    DpuExtensionServiceInterface
	NVLinkLogicalPartition NVLinkLogicalPartitionInterface
	FlowGrpc               FlowGrpcInterface
	TenantIdentity         TenantIdentityInterface
}

// ManagerConf - Conf struct
type ManagerConf struct {
	EB *conftypes.Config
}
