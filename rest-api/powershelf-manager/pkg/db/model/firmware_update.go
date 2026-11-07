// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"errors"
	"net"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"

	"github.com/uptrace/bun"
)

// FirmwareUpdate represents the latest firmware update operation for a specific
// component of a PMC (Power Management Controller). The composite primary key is
// (PmcMacAddress, Component).
type FirmwareUpdate struct {
	bun.BaseModel `bun:"table:firmware_update,alias:fu"`

	PmcMacAddress      MacAddr                  `bun:"pmc_mac_address,pk,notnull,type:macaddr"` // MAC address of the target PMC (FK to pmc.mac_address)
	Component          powershelf.Component     `bun:"component,pk,notnull"`                    // Component being updated (e.g., "PMC", "PSU1")
	VersionFrom        string                   `bun:"version_from,notnull"`                    // Firmware version before upgrade
	VersionTo          string                   `bun:"version_to,notnull"`                      // Target firmware version after upgrade
	State              powershelf.FirmwareState `bun:"state,notnull"`                           // Upgrade state ("Queued", "Updating", "Completed", "Failed", etc.)
	LastTransitionTime time.Time                `bun:"last_transition_time,notnull"`            // When the state last changed
	JobID              string                   `bun:"job_id"`                                  // Device job/task ID, if provided by hardware
	ErrorMessage       string                   `bun:"error_message"`                           // Error message if the upgrade failed
	CreatedAt          time.Time                `bun:"created_at,notnull,default:now()"`        // When this record was created
	UpdatedAt          time.Time                `bun:"updated_at,notnull,default:now()"`        // When this record was last updated
}

// NewFirmwareUpdate creates and inserts (or upserts) a FirmwareUpdate record using the composite PK.
// If a record already exists for (pmcMac, comp), it will be replaced.
func NewFirmwareUpdate(ctx context.Context, db bun.IDB, pmcMac net.HardwareAddr, comp powershelf.Component, vStart, vTarget string) (*FirmwareUpdate, error) {
	now := time.Now()
	fu := &FirmwareUpdate{
		PmcMacAddress:      MacAddr(pmcMac),
		Component:          comp,
		VersionFrom:        vStart,
		VersionTo:          vTarget,
		State:              powershelf.FirmwareStateQueued,
		LastTransitionTime: now,
		JobID:              "",
		CreatedAt:          now,
		UpdatedAt:          now,
	}

	_, err := db.NewInsert().
		Model(fu).
		On("CONFLICT (pmc_mac_address, component) DO UPDATE").
		Set("version_from = EXCLUDED.version_from, version_to = EXCLUDED.version_to, state = EXCLUDED.state, last_transition_time = EXCLUDED.last_transition_time, job_id = EXCLUDED.job_id, error_message = EXCLUDED.error_message, updated_at = EXCLUDED.updated_at").
		Exec(ctx)

	return fu, err
}

// GetFirmwareUpdate fetches a FirmwareUpdate by composite PK (pmcMac, comp).
func GetFirmwareUpdate(ctx context.Context, db bun.IDB, pmcMac net.HardwareAddr, comp powershelf.Component) (*FirmwareUpdate, error) {
	var fu FirmwareUpdate
	err := db.NewSelect().
		Model(&fu).
		Where("pmc_mac_address = ? AND component = ?", MacAddr(pmcMac), comp).
		Scan(ctx)
	if err != nil {
		return nil, err
	}
	return &fu, nil
}

// ListFirmwareUpdatesForPMC lists all firmware updates for a given PMC (optionally filter by component).
func ListFirmwareUpdatesForPMC(ctx context.Context, db bun.IDB, pmcMac net.HardwareAddr, comp *powershelf.Component) ([]FirmwareUpdate, error) {
	var updates []FirmwareUpdate
	q := db.NewSelect().Model(&updates).Where("pmc_mac_address = ?", MacAddr(pmcMac))
	if comp != nil {
		q = q.Where("component = ?", *comp)
	}
	err := q.Order("created_at DESC").Scan(ctx)
	return updates, err
}

// Create inserts a new FirmwareUpdate row.
func (fu *FirmwareUpdate) Create(ctx context.Context, tx bun.Tx) error {
	_, err := tx.NewInsert().Model(fu).Exec(ctx)
	return err
}

// Get retrieves a FirmwareUpdate by MAC and component (both must be specified).
func (fu *FirmwareUpdate) Get(
	ctx context.Context,
	idb bun.IDB,
) (*FirmwareUpdate, error) {
	var retFwUpdate FirmwareUpdate
	var query *bun.SelectQuery

	if fu.PmcMacAddress != nil && fu.Component != "" {
		query = idb.NewSelect().Model(&retFwUpdate).Where("pmc_mac_address = ? AND component = ?", fu.PmcMacAddress, fu.Component)
	} else {
		return nil, errors.New("cannot query fw updates without specifying a PMC MAC address and a component")
	}

	if err := query.Scan(ctx); err != nil {
		return nil, err
	}

	return &retFwUpdate, nil
}

// SetFirmwareUpdateState transitions a firmware update identified by (pmcMac, comp)
// to newState with an optional error message in a single UPDATE (no prior SELECT).
func SetFirmwareUpdateState(ctx context.Context, db bun.IDB, pmcMac net.HardwareAddr, comp powershelf.Component, newState powershelf.FirmwareState, errMsg string) error {
	now := time.Now()
	fu := &FirmwareUpdate{
		PmcMacAddress:      MacAddr(pmcMac),
		Component:          comp,
		State:              newState,
		ErrorMessage:       errMsg,
		LastTransitionTime: now,
		UpdatedAt:          now,
	}

	_, err := db.NewUpdate().
		Model(fu).
		Column("state", "last_transition_time", "error_message", "updated_at").
		WherePK().
		Exec(ctx)
	return err
}

// UpdateFirmwareUpdateState sets the state and optional error message for a FirmwareUpdate.
// Only updates LastTransitionTime if the state actually changes.
func (fu *FirmwareUpdate) UpdateFirmwareUpdateState(ctx context.Context, db bun.IDB, newState powershelf.FirmwareState, errMsg string) error {
	if fu.State == newState && fu.ErrorMessage == errMsg {
		// No change; avoid unnecessary DB write.
		return nil
	}
	now := time.Now()
	if fu.State != newState {
		fu.State = newState
		fu.LastTransitionTime = now
	}
	fu.ErrorMessage = errMsg
	fu.UpdatedAt = now
	_, err := db.NewUpdate().
		Model(fu).
		Column("state", "last_transition_time", "error_message", "updated_at").
		WherePK().
		Exec(ctx)
	return err
}

// SetJobID sets the job ID for a FirmwareUpdate and persists it.
func (fu *FirmwareUpdate) SetJobID(ctx context.Context, db bun.IDB, jobID string) error {
	if fu.JobID == jobID {
		return nil
	}
	fu.JobID = jobID
	fu.UpdatedAt = time.Now()
	_, err := db.NewUpdate().
		Model(fu).
		Column("job_id", "updated_at").
		WherePK().
		Exec(ctx)
	return err
}

// SetVersionTarget sets the target version for a FirmwareUpdate and persists it.
func (fu *FirmwareUpdate) SetVersionTarget(ctx context.Context, db bun.IDB, vTarget string) error {
	if fu.VersionTo == vTarget {
		return nil
	}
	fu.VersionTo = vTarget
	fu.UpdatedAt = time.Now()
	_, err := db.NewUpdate().
		Model(fu).
		Column("version_to", "updated_at").
		WherePK().
		Exec(ctx)
	return err
}

// DeleteFirmwareUpdate deletes a FirmwareUpdate by composite PK.
func DeleteFirmwareUpdate(ctx context.Context, db bun.IDB, pmcMac net.HardwareAddr, comp powershelf.Component) error {
	_, err := db.NewDelete().
		Model((*FirmwareUpdate)(nil)).
		Where("pmc_mac_address = ? AND component = ?", MacAddr(pmcMac), comp).
		Exec(ctx)
	return err
}

// IsTerminal returns true if the firmware update is in a terminal state.
func (fu *FirmwareUpdate) IsTerminal() bool {
	return fu.State == powershelf.FirmwareStateCompleted || fu.State == powershelf.FirmwareStateFailed
}

// GetAllPendingFirmwareUpdates lists all non-terminal firmware updates across all PMCs
func GetAllPendingFirmwareUpdates(ctx context.Context, db bun.IDB) ([]FirmwareUpdate, error) {
	var updates []FirmwareUpdate
	err := db.NewSelect().
		Model(&updates).
		Where("state NOT IN (?, ?)", powershelf.FirmwareStateCompleted, powershelf.FirmwareStateFailed).
		Order("created_at DESC").
		Scan(ctx)
	return updates, err
}
