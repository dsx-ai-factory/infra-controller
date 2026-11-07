// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// MachineExpansion - Machine Expansion
type MachineExpansion interface{}

// MachineInterface - interface to Machine
type MachineInterface interface {
	// List all the apis of Machine here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error

	GetState() []string

	MachineExpansion
}
