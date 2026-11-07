// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitchregistry

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
)

// Registry defines the interface for NV-Switch tray storage.
type Registry interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error

	// Register creates a new NV-Switch entry or updates an existing one.
	// Returns the UUID and whether it was newly created.
	Register(ctx context.Context, tray *nvswitch.NVSwitchTray) (uuid.UUID, bool, error)

	// Get retrieves an NV-Switch by UUID.
	Get(ctx context.Context, id uuid.UUID) (*nvswitch.NVSwitchTray, error)

	// GetByBMCMAC retrieves an NV-Switch by BMC MAC address.
	GetByBMCMAC(ctx context.Context, bmcMAC string) (*nvswitch.NVSwitchTray, error)

	// List returns all registered NV-Switches.
	List(ctx context.Context) ([]*nvswitch.NVSwitchTray, error)

	// Delete removes an NV-Switch by UUID.
	Delete(ctx context.Context, id uuid.UUID) error
}
