// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// NetworkSecurityGroupExpansion - NetworkSecurityGroup Expansion
type NetworkSecurityGroupExpansion interface{}

// NetworkSecurityGroupInterface - Interface for NetworkSecurityGroup
type NetworkSecurityGroupInterface interface {
	// List all the APIs for NetworkSecurityGroup here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	NetworkSecurityGroupExpansion
}
