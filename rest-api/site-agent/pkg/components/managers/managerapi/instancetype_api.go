// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// InstanceTypeExpansion - InstanceType Expansion
type InstanceTypeExpansion interface{}

// InstanceTypeInterface - Interface for InstanceType
type InstanceTypeInterface interface {
	// List all the APIs for InstanceType here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	InstanceTypeExpansion
}
