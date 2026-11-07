// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// ExpectedMachineExpansion - ExpectedMachine Expansion
type ExpectedMachineExpansion interface{}

// ExpectedMachineInterface - interface to ExpectedMachine
type ExpectedMachineInterface interface {
	// List all the apis of ExpectedMachine here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	ExpectedMachineExpansion
}
