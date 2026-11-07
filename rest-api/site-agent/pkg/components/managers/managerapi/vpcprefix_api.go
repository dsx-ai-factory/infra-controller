// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// VpcPrefixExpansion - VpcPrefix Expansion
type VpcPrefixExpansion interface{}

// VpcPrefixInterface - Interface for VpcPrefix
type VpcPrefixInterface interface {
	// List all the APIs for VpcPrefix here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	VpcPrefixExpansion
}
