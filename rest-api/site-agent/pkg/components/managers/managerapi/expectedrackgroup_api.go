// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// ExpectedRackGroupExpansion - ExpectedRackGroup Expansion
type ExpectedRackGroupExpansion interface{}

// ExpectedRackGroupInterface - interface to ExpectedRackGroup
type ExpectedRackGroupInterface interface {
	// List all the apis of ExpectedRackGroup here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	ExpectedRackGroupExpansion
}
