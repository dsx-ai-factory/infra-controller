// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// VpcPeeringExpansion - VpcPeering expansion hook for future APIs
type VpcPeeringExpansion interface{}

// VpcPeeringInterface - Interface for VPC Peering site-agent manager
type VpcPeeringInterface interface {
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	VpcPeeringExpansion
}
