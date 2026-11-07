// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

// Ensure PostgresUpdateStore implements UpdateStore.
var _ UpdateStore = (*PostgresUpdateStore)(nil)

// PostgresUpdateStore implements UpdateStore using PostgreSQL.
type PostgresUpdateStore struct {
	db *bun.DB
}

// NewPostgresUpdateStore creates a new PostgreSQL-backed update store.
func NewPostgresUpdateStore(db *bun.DB) *PostgresUpdateStore {
	return &PostgresUpdateStore{db: db}
}

// FirmwareUpdateModel is the database model for firmware updates.
// This maps to the firmware_update table.
type FirmwareUpdateModel struct {
	bun.BaseModel `bun:"table:firmware_update,alias:fu"`

	ID            uuid.UUID          `bun:"id,pk,type:uuid"`
	SwitchUUID    uuid.UUID          `bun:"switch_uuid,notnull,type:uuid"`
	Component     nvswitch.Component `bun:"component,notnull"`
	BundleVersion string             `bun:"bundle_version,notnull"`
	Strategy      Strategy           `bun:"strategy,notnull"`
	State         UpdateState        `bun:"state,notnull"`
	VersionFrom   string             `bun:"version_from"`
	VersionTo     string             `bun:"version_to,notnull"`
	VersionActual string             `bun:"version_actual"`
	TaskURI       string             `bun:"task_uri"`
	ErrorMessage  string             `bun:"error_message"`
	// Sequencing fields for multi-component updates
	BundleUpdateID *uuid.UUID `bun:"bundle_update_id,type:uuid"`
	SequenceOrder  int        `bun:"sequence_order"`
	PredecessorID  *uuid.UUID `bun:"predecessor_id,type:uuid"`
	// Async worker pool fields
	ExecContext   *ExecContext `bun:"exec_context,type:jsonb"`
	LastCheckedAt *time.Time   `bun:"last_checked_at"`
	// Timestamps
	CreatedAt time.Time `bun:"created_at,notnull,default:now()"`
	UpdatedAt time.Time `bun:"updated_at,notnull,default:now()"`
}

// toModel converts a FirmwareUpdate to its database model.
func toModel(fu *FirmwareUpdate) *FirmwareUpdateModel {
	return &FirmwareUpdateModel{
		ID:             fu.ID,
		SwitchUUID:     fu.SwitchUUID,
		Component:      fu.Component,
		BundleVersion:  fu.BundleVersion,
		Strategy:       fu.Strategy,
		State:          fu.State,
		VersionFrom:    fu.VersionFrom,
		VersionTo:      fu.VersionTo,
		VersionActual:  fu.VersionActual,
		TaskURI:        fu.TaskURI,
		ErrorMessage:   fu.ErrorMessage,
		BundleUpdateID: fu.BundleUpdateID,
		SequenceOrder:  fu.SequenceOrder,
		PredecessorID:  fu.PredecessorID,
		ExecContext:    fu.ExecContext,
		LastCheckedAt:  fu.LastCheckedAt,
		CreatedAt:      fu.CreatedAt,
		UpdatedAt:      fu.UpdatedAt,
	}
}

// fromModel converts a database model to FirmwareUpdate.
func fromModel(m *FirmwareUpdateModel) *FirmwareUpdate {
	return &FirmwareUpdate{
		ID:             m.ID,
		SwitchUUID:     m.SwitchUUID,
		Component:      m.Component,
		BundleVersion:  m.BundleVersion,
		Strategy:       m.Strategy,
		State:          m.State,
		VersionFrom:    m.VersionFrom,
		VersionTo:      m.VersionTo,
		VersionActual:  m.VersionActual,
		TaskURI:        m.TaskURI,
		ErrorMessage:   m.ErrorMessage,
		BundleUpdateID: m.BundleUpdateID,
		SequenceOrder:  m.SequenceOrder,
		PredecessorID:  m.PredecessorID,
		ExecContext:    m.ExecContext,
		LastCheckedAt:  m.LastCheckedAt,
		CreatedAt:      m.CreatedAt,
		UpdatedAt:      m.UpdatedAt,
	}
}

// Save persists a firmware update (insert or update).
func (s *PostgresUpdateStore) Save(ctx context.Context, update *FirmwareUpdate) error {
	update.UpdatedAt = time.Now()
	model := toModel(update)

	_, err := s.db.NewInsert().
		Model(model).
		On("CONFLICT (id) DO UPDATE").
		Set("state = EXCLUDED.state").
		Set("version_from = EXCLUDED.version_from").
		Set("version_actual = EXCLUDED.version_actual").
		Set("task_uri = EXCLUDED.task_uri").
		Set("error_message = EXCLUDED.error_message").
		Set("exec_context = EXCLUDED.exec_context").
		Set("last_checked_at = EXCLUDED.last_checked_at").
		Set("updated_at = EXCLUDED.updated_at").
		Exec(ctx)

	return err
}

// Get retrieves a firmware update by ID.
func (s *PostgresUpdateStore) Get(ctx context.Context, id uuid.UUID) (*FirmwareUpdate, error) {
	var model FirmwareUpdateModel
	err := s.db.NewSelect().
		Model(&model).
		Where("id = ?", id).
		Scan(ctx)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, ErrUpdateNotFound
		}
		return nil, err
	}

	return fromModel(&model), nil
}

// GetAll returns all firmware updates.
func (s *PostgresUpdateStore) GetAll(ctx context.Context) ([]*FirmwareUpdate, error) {
	var models []FirmwareUpdateModel
	err := s.db.NewSelect().
		Model(&models).
		Order("created_at DESC").
		Scan(ctx)

	if err != nil {
		return nil, err
	}

	updates := make([]*FirmwareUpdate, len(models))
	for i, m := range models {
		updates[i] = fromModel(&m)
	}
	return updates, nil
}

// GetPendingUpdates returns up to `limit` updates that need processing.
// Returns both:
//   - QUEUED updates whose predecessor has completed (or has no predecessor)
//   - Active (non-terminal, non-queued) updates
//
// Priority: QUEUED first (by sequence_order, created_at), then active (by oldest updated_at).
// This method does NOT modify any state - state transitions happen in the worker.
func (s *PostgresUpdateStore) GetPendingUpdates(ctx context.Context, limit int) ([]*FirmwareUpdate, error) {
	var results []*FirmwareUpdate

	// First, get QUEUED updates ready to start (predecessor completed or no predecessor)
	var queuedModels []FirmwareUpdateModel
	err := s.db.NewSelect().
		Model(&queuedModels).
		Where("state = ?", StateQueued).
		Where(`(
			predecessor_id IS NULL 
			OR predecessor_id IN (SELECT id FROM firmware_update WHERE state = ?)
		)`, StateCompleted).
		OrderExpr("sequence_order ASC, created_at ASC").
		Limit(limit).
		Scan(ctx)

	if err != nil && err != sql.ErrNoRows {
		return nil, err
	}

	for i := range queuedModels {
		results = append(results, fromModel(&queuedModels[i]))
	}

	// If we have enough, return early
	remaining := limit - len(results)
	if remaining <= 0 {
		return results, nil
	}

	// Get active updates (non-terminal, non-queued), ordered by oldest updated_at
	var activeModels []FirmwareUpdateModel
	err = s.db.NewSelect().
		Model(&activeModels).
		Where("state NOT IN (?, ?, ?, ?)", StateQueued, StateCompleted, StateFailed, StateCancelled).
		OrderExpr("updated_at ASC NULLS FIRST").
		Limit(remaining).
		Scan(ctx)

	if err != nil && err != sql.ErrNoRows {
		return nil, err
	}

	for i := range activeModels {
		results = append(results, fromModel(&activeModels[i]))
	}

	return results, nil
}

// GetLatestBundleBySwitch returns firmware updates belonging to the most
// recent bundle_update_id for the given switch. For single-component
// updates (bundle_update_id IS NULL) it returns only the newest record.
// This avoids stale historical failures from previous update attempts.
func (s *PostgresUpdateStore) GetLatestBundleBySwitch(ctx context.Context, switchUUID uuid.UUID) ([]*FirmwareUpdate, error) {
	var models []FirmwareUpdateModel
	err := s.db.NewSelect().
		Model(&models).
		Where("switch_uuid = ?", switchUUID).
		Where(`(
			bundle_update_id = (
				SELECT bundle_update_id FROM firmware_update
				WHERE switch_uuid = ? AND bundle_update_id IS NOT NULL
				ORDER BY created_at DESC LIMIT 1
			)
			OR (
				bundle_update_id IS NULL AND id = (
					SELECT id FROM firmware_update
					WHERE switch_uuid = ? AND bundle_update_id IS NULL
					ORDER BY created_at DESC LIMIT 1
				)
			)
		)`, switchUUID, switchUUID).
		Order("created_at DESC").
		Scan(ctx)

	if err != nil {
		return nil, err
	}

	updates := make([]*FirmwareUpdate, len(models))
	for i, m := range models {
		updates[i] = fromModel(&m)
	}
	return updates, nil
}

// GetActive returns the active (non-terminal) update for a switch/component pair.
func (s *PostgresUpdateStore) GetActive(ctx context.Context, switchUUID uuid.UUID, component nvswitch.Component) (*FirmwareUpdate, error) {
	var model FirmwareUpdateModel
	err := s.db.NewSelect().
		Model(&model).
		Where("switch_uuid = ?", switchUUID).
		Where("component = ?", component).
		Where("state NOT IN (?, ?, ?)", StateCompleted, StateFailed, StateCancelled).
		Scan(ctx)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, nil
		}
		return nil, err
	}

	return fromModel(&model), nil
}

// GetAnyActiveForSwitch returns any active (non-terminal) update for a switch.
func (s *PostgresUpdateStore) GetAnyActiveForSwitch(ctx context.Context, switchUUID uuid.UUID) (*FirmwareUpdate, error) {
	var model FirmwareUpdateModel
	err := s.db.NewSelect().
		Model(&model).
		Where("switch_uuid = ?", switchUUID).
		Where("state NOT IN (?, ?, ?)", StateCompleted, StateFailed, StateCancelled).
		Limit(1).
		Scan(ctx)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, nil
		}
		return nil, err
	}

	return fromModel(&model), nil
}

// CancelRemainingInBundle cancels all QUEUED updates in a bundle that come after
// the specified sequence order. Used when a predecessor fails.
func (s *PostgresUpdateStore) CancelRemainingInBundle(ctx context.Context, bundleUpdateID uuid.UUID, afterSequence int, failedComponent nvswitch.Component) (int, error) {
	errorMsg := fmt.Sprintf("cancelled: predecessor %s update failed", failedComponent)

	result, err := s.db.NewUpdate().
		Model((*FirmwareUpdateModel)(nil)).
		Set("state = ?", StateCancelled).
		Set("error_message = ?", errorMsg).
		Set("updated_at = ?", time.Now()).
		Where("bundle_update_id = ?", bundleUpdateID).
		Where("sequence_order > ?", afterSequence).
		Where("state = ?", StateQueued).
		Exec(ctx)

	if err != nil {
		return 0, err
	}

	rowsAffected, _ := result.RowsAffected()
	return int(rowsAffected), nil
}

// Delete removes a firmware update by ID.
func (s *PostgresUpdateStore) Delete(ctx context.Context, id uuid.UUID) error {
	_, err := s.db.NewDelete().
		Model((*FirmwareUpdateModel)(nil)).
		Where("id = ?", id).
		Exec(ctx)
	return err
}
