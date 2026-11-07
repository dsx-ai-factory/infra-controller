// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// ExpectedSwitchExpansion - ExpectedSwitch Expansion
type ExpectedSwitchExpansion interface{}

// ExpectedSwitchInterface - interface to ExpectedSwitch
type ExpectedSwitchInterface interface {
	// List all the apis of ExpectedSwitch here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	ExpectedSwitchExpansion
}
