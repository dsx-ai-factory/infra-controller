// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"errors"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
)

// ErrNoWorkAvailable is returned when no work items are available for processing.
var ErrNoWorkAvailable = errors.New("no work available")

// ErrUpdateNotFound is returned when an update is not found.
var ErrUpdateNotFound = errors.New("firmware update not found")

// UpdateStore provides persistence for firmware update records.
type UpdateStore interface {
	// Save persists a firmware update (insert or update).
	Save(ctx context.Context, update *FirmwareUpdate) error

	// Get retrieves a firmware update by ID.
	Get(ctx context.Context, id uuid.UUID) (*FirmwareUpdate, error)

	// GetAll returns all firmware updates.
	GetAll(ctx context.Context) ([]*FirmwareUpdate, error)

	// GetPendingUpdates returns up to `limit` updates that need processing.
	// Priority:
	//   1. QUEUED updates whose predecessor has completed (or has no predecessor)
	//   2. Active (non-terminal, non-queued) updates, ordered by oldest updated_at first
	//
	// For QUEUED updates, the state is transitioned to the first active state before returning.
	// This method is used by the scheduler to dispatch work to workers.
	GetPendingUpdates(ctx context.Context, limit int) ([]*FirmwareUpdate, error)

	// GetLatestBundleBySwitch returns firmware updates for the most recent
	// bundle_update_id of a given switch. This prevents stale historical
	// records (e.g. old CPLD failures) from polluting the current status.
	GetLatestBundleBySwitch(ctx context.Context, switchUUID uuid.UUID) ([]*FirmwareUpdate, error)

	// GetActive returns the active (non-terminal) update for a switch/component pair.
	// Returns nil, nil if no active update exists.
	GetActive(ctx context.Context, switchUUID uuid.UUID, component nvswitch.Component) (*FirmwareUpdate, error)

	// GetAnyActiveForSwitch returns any active (non-terminal) update for a switch.
	// Returns nil, nil if no active update exists.
	GetAnyActiveForSwitch(ctx context.Context, switchUUID uuid.UUID) (*FirmwareUpdate, error)

	// CancelRemainingInBundle cancels all QUEUED updates in a bundle that come after
	// the specified sequence order. Used when a predecessor fails.
	// Returns the number of updates cancelled.
	CancelRemainingInBundle(ctx context.Context, bundleUpdateID uuid.UUID, afterSequence int, failedComponent nvswitch.Component) (int, error)

	// Delete removes a firmware update by ID.
	Delete(ctx context.Context, id uuid.UUID) error
}
