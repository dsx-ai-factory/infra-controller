// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// SKUExpansion - SKU Expansion
type SKUExpansion interface{}

// SKUInterface - interface to SKU
type SKUInterface interface {
	// List all the apis of SKU here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	SKUExpansion
}
