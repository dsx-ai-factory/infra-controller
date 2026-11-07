// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// TenantIdentityExpansion - TenantIdentity Expansion
type TenantIdentityExpansion interface{}

// TenantIdentityInterface - Interface for TenantIdentity
type TenantIdentityInterface interface {
	// List all the APIs for TenantIdentity here
	Init()
	RegisterSubscriber() error
	GetState() []string
	TenantIdentityExpansion
}
