// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package roles defines the canonical authorization role suffix names
// used throughout the codebase. It deliberately has no dependencies so it
// can be imported from any package — including db model tests (which would
// otherwise create an import cycle through auth/pkg/authorization) and
// from production binaries that don't ship the auth module.
package roles

const (
	// ProviderAdminRole is the suffix for the provider admin authorization role.
	ProviderAdminRole = "PROVIDER_ADMIN"

	// ProviderViewerRole is the suffix for the provider viewer authorization role.
	ProviderViewerRole = "PROVIDER_VIEWER"

	// TenantAdminRole is the suffix for the tenant admin authorization role.
	TenantAdminRole = "TENANT_ADMIN"
)
