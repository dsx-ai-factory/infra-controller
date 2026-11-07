// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// TenantExpansion - Tenant Expansion
type TenantExpansion interface{}

// TenantInterface - Interface for Tenant
type TenantInterface interface {
	// List all the APIs for Tenant here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	TenantExpansion
}
