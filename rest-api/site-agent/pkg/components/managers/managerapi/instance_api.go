// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// InstanceExpansion - Instance Expansion
type InstanceExpansion interface{}

// InstanceInterface - interface to Instance
type InstanceInterface interface {
	// List all the apis of Instance here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	InstanceExpansion
}
