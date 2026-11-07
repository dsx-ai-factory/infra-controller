// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// MachineValidationExpansion - MachineValidation Expansion
type MachineValidationExpansion interface{}

// MachineValidationInterface - Interface for MachineValidation
type MachineValidationInterface interface {
	// List all the APIs for MachineValidation here
	Init()
	RegisterSubscriber() error
	GetState() []string
	MachineValidationExpansion
}
