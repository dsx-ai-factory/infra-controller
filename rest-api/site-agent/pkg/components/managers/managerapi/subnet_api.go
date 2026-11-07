// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// SubnetExpansion - Subnet Expansion
type SubnetExpansion interface{}

// SubnetInterface - interface to Subnet
type SubnetInterface interface {
	// List all the apis of Subnet here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	SubnetExpansion
}
