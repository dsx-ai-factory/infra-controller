// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// ExpectedRackExpansion - ExpectedRack Expansion
type ExpectedRackExpansion interface{}

// ExpectedRackInterface - interface to ExpectedRack
type ExpectedRackInterface interface {
	// List all the apis of ExpectedRack here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	ExpectedRackExpansion
}
