// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// OperatingSystemExpansion - Operating System Expansion
type OperatingSystemExpansion interface{}

// OperatingSystemInterface - Interface for Operating System
type OperatingSystemInterface interface {
	// List all the APIs for Operating System here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	OperatingSystemExpansion
}
