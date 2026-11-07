// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// ExpectedPowerShelfExpansion - ExpectedPowerShelf Expansion
type ExpectedPowerShelfExpansion interface{}

// ExpectedPowerShelfInterface - interface to ExpectedPowerShelf
type ExpectedPowerShelfInterface interface {
	// List all the apis of ExpectedPowerShelf here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	ExpectedPowerShelfExpansion
}
