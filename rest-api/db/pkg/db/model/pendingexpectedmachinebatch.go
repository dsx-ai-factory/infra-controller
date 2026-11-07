// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// PendingExpectedMachineBatch reserves identities until Core creation is confirmed or rejected.
// Inputs exclude credentials; an inconclusive execution must retain its reservation.
type PendingExpectedMachineBatch struct {
	WorkflowID   string
	RunID        *string
	SiteID       uuid.UUID
	Inputs       []ExpectedMachineCreateInput
	Created      time.Time
	LastObserved *time.Time
}

// PendingExpectedMachineBatchStats summarizes unresolved reservations for a Site.
type PendingExpectedMachineBatchStats struct {
	SiteID        uuid.UUID
	SiteName      string
	BatchCount    int64
	OldestCreated *time.Time
}

// PendingExpectedMachine keeps one private batch member and its SKU reference.
// Its foreign key protects the local REST SKU record until the public row is committed
// or creation is rejected. Workflow members are inserted and removed together.
type PendingExpectedMachine struct {
	bun.BaseModel `bun:"table:pending_expected_machine,alias:pem"`
	ID            uuid.UUID                  `bun:"id,pk,type:uuid"`
	SiteID        uuid.UUID                  `bun:"site_id,type:uuid,notnull,unique:pending_expected_machine_site_workflow_position_key"`
	WorkflowID    string                     `bun:"workflow_id,notnull,unique:pending_expected_machine_site_workflow_position_key"`
	RunID         *string                    `bun:"run_id"`
	SkuID         *string                    `bun:"sku_id"`
	Position      int                        `bun:"position,notnull,unique:pending_expected_machine_site_workflow_position_key"`
	Input         ExpectedMachineCreateInput `bun:"input,type:jsonb,notnull"`
	Created       time.Time                  `bun:"created,nullzero,notnull,default:current_timestamp"`
	LastObserved  *time.Time                 `bun:"last_observed"`
}

// BeforeCreateTable preserves pending SKU references in schemas built from models.
func (p *PendingExpectedMachine) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("sku_id") REFERENCES "sku" ("id")`)
	return nil
}

// Overlaps reports whether a mutation touches any reserved UUID, MAC, or serial.
// An empty selector denotes a complete Site mutation. Stored serials remain case-sensitive.
func (b PendingExpectedMachineBatch) Overlaps(ids []uuid.UUID, macs, serials []string) bool {
	if len(ids) == 0 && len(macs) == 0 && len(serials) == 0 {
		return true
	}
	for _, input := range b.Inputs {
		if slices.Contains(ids, input.ExpectedMachineID) || slices.Contains(serials, input.ChassisSerialNumber) ||
			slices.ContainsFunc(macs, func(mac string) bool {
				requested, requestedErr := net.ParseMAC(mac)
				reserved, reservedErr := net.ParseMAC(input.BmcMacAddress)
				if requestedErr == nil && reservedErr == nil {
					return bytes.Equal(requested, reserved)
				}
				return strings.EqualFold(mac, input.BmcMacAddress)
			}) {
			return true
		}
	}
	return false
}

// Succeeded requires one successful result for every reserved UUID.
// Partial, malformed, or unrelated responses leave the batch unresolved.
func (b PendingExpectedMachineBatch) Succeeded(response *corev1.BatchExpectedMachineOperationResponse) bool {
	if len(b.Inputs) == 0 || len(response.GetResults()) != len(b.Inputs) {
		return false
	}
	remaining := make(map[uuid.UUID]bool, len(b.Inputs))
	for _, input := range b.Inputs {
		remaining[input.ExpectedMachineID] = true
	}
	for _, result := range response.GetResults() {
		id, err := uuid.Parse(result.GetId().GetValue())
		if err != nil || !result.GetSuccess() || !remaining[id] {
			return false
		}
		delete(remaining, id)
	}
	return len(remaining) == 0
}

// PendingExpectedMachineBatchDAO owns private reservations. Admission and
// resolution hold the Site inventory lock; observation metadata updates do not.
// Site teardown removes groups after excluding live Site writers.
type PendingExpectedMachineBatchDAO interface {
	Create(ctx context.Context, tx *db.Tx, batch PendingExpectedMachineBatch) error
	GetAll(ctx context.Context, tx *db.Tx, siteID uuid.UUID) ([]PendingExpectedMachineBatch, error)
	GetStatistics(ctx context.Context, tx *db.Tx) ([]PendingExpectedMachineBatchStats, error)
	BindRunID(ctx context.Context, siteID uuid.UUID, workflowID, runID string) error
	MarkObserved(ctx context.Context, siteID uuid.UUID, workflowID string) (bool, error)
	Contains(ctx context.Context, tx *db.Tx, siteID, machineID uuid.UUID) (bool, error)
	Delete(ctx context.Context, tx *db.Tx, siteID uuid.UUID, workflowID string) error
	DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error
	Complete(ctx context.Context, tx *db.Tx, siteID uuid.UUID, workflowID string) ([]ExpectedMachine, error)
}

// PendingExpectedMachineBatchSQLDAO implements PendingExpectedMachineBatchDAO.
type PendingExpectedMachineBatchSQLDAO struct{ dbSession *db.Session }

// NewPendingExpectedMachineBatchDAO returns the private batch persistence owner.
func NewPendingExpectedMachineBatchDAO(session *db.Session) PendingExpectedMachineBatchDAO {
	return &PendingExpectedMachineBatchSQLDAO{dbSession: session}
}

// Create persists nonsecret inputs before the workflow is dispatched.
func (d *PendingExpectedMachineBatchSQLDAO) Create(ctx context.Context, tx *db.Tx, batch PendingExpectedMachineBatch) error {
	if batch.WorkflowID == "" || len(batch.Inputs) == 0 {
		return errors.New("pending Expected Machine batch requires a workflow and members")
	}
	rows := make([]PendingExpectedMachine, 0, len(batch.Inputs))
	for position, input := range batch.Inputs {
		if input.SiteID != batch.SiteID {
			return errors.New("pending Expected Machine batch contains a different Site")
		}
		rows = append(rows, PendingExpectedMachine{ID: input.ExpectedMachineID, WorkflowID: batch.WorkflowID, RunID: batch.RunID, SiteID: batch.SiteID, SkuID: input.SkuID, Position: position, Input: input, Created: batch.Created})
	}
	_, err := db.GetIDB(tx, d.dbSession).NewInsert().Model(&rows).Exec(ctx)
	return err
}

// GetAll returns least recently observed batches first, preserving member order.
// New batches precede observed batches so a lost result cannot starve later work.
func (d *PendingExpectedMachineBatchSQLDAO) GetAll(ctx context.Context, tx *db.Tx, siteID uuid.UUID) ([]PendingExpectedMachineBatch, error) {
	var rows []PendingExpectedMachine
	err := db.GetIDB(tx, d.dbSession).NewSelect().Model(&rows).Where("site_id = ?", siteID).OrderExpr("last_observed ASC NULLS FIRST").Order("created", "workflow_id", "position").Scan(ctx)
	if err != nil {
		return nil, err
	}
	batches := make([]PendingExpectedMachineBatch, 0)
	indexByWorkflow := make(map[string]int)
	for _, row := range rows {
		index, found := indexByWorkflow[row.WorkflowID]
		if !found {
			index = len(batches)
			indexByWorkflow[row.WorkflowID] = index
			batches = append(batches, PendingExpectedMachineBatch{WorkflowID: row.WorkflowID, RunID: row.RunID, SiteID: row.SiteID, Created: row.Created, LastObserved: row.LastObserved})
		}
		batches[index].Inputs = append(batches[index].Inputs, row.Input)
	}
	return batches, nil
}

// GetStatistics includes every nondeleted Site, even when it has no reservations.
// A workflow reserves one row per machine, but counts as a single batch.
func (d *PendingExpectedMachineBatchSQLDAO) GetStatistics(ctx context.Context, tx *db.Tx) ([]PendingExpectedMachineBatchStats, error) {
	var stats []PendingExpectedMachineBatchStats
	err := db.GetIDB(tx, d.dbSession).NewSelect().TableExpr("site AS st").
		ColumnExpr("st.id AS site_id, st.name AS site_name").
		ColumnExpr("COUNT(DISTINCT pem.workflow_id) AS batch_count, MIN(pem.created) AS oldest_created").
		Join("LEFT JOIN pending_expected_machine AS pem ON pem.site_id = st.id").
		Where("st.deleted IS NULL").Group("st.id", "st.name").Scan(ctx, &stats)
	return stats, err
}

// BindRunID records the acknowledged original execution without replacing an
// existing binding. Concurrent resolution may already have removed the group.
func (d *PendingExpectedMachineBatchSQLDAO) BindRunID(ctx context.Context, siteID uuid.UUID, workflowID, runID string) error {
	if runID == "" {
		return errors.New("pending Expected Machine batch requires a nonempty run ID")
	}
	_, err := d.dbSession.DB.NewUpdate().Model((*PendingExpectedMachine)(nil)).
		Set("run_id = COALESCE(run_id, ?)", runID).Where("site_id = ?", siteID).Where("workflow_id = ?", workflowID).Exec(ctx)
	return err
}

// MarkObserved advances every member before observation or positive completion.
// A later activity considers other groups first even when this attempt fails.
// False means the group was already resolved by another caller.
func (d *PendingExpectedMachineBatchSQLDAO) MarkObserved(ctx context.Context, siteID uuid.UUID, workflowID string) (bool, error) {
	result, err := d.dbSession.DB.NewUpdate().Model((*PendingExpectedMachine)(nil)).
		Set("last_observed = CURRENT_TIMESTAMP").Where("site_id = ?", siteID).Where("workflow_id = ?", workflowID).Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count > 0, err
}

// Contains reports whether this exact machine belongs to a pending group on the Site.
// Stale inventory deletion calls it under the shared Site inventory lock.
func (d *PendingExpectedMachineBatchSQLDAO) Contains(ctx context.Context, tx *db.Tx, siteID, machineID uuid.UUID) (bool, error) {
	return db.GetIDB(tx, d.dbSession).NewSelect().Model((*PendingExpectedMachine)(nil)).
		Where("site_id = ?", siteID).Where("id = ?", machineID).Exists(ctx)
}

// Delete releases every member after a definitive rejection. The caller holds
// the exclusive Site inventory lock so admission cannot miss the release.
func (d *PendingExpectedMachineBatchSQLDAO) Delete(ctx context.Context, tx *db.Tx, siteID uuid.UUID, workflowID string) error {
	_, err := db.GetIDB(tx, d.dbSession).NewDelete().Model((*PendingExpectedMachine)(nil)).Where("site_id = ?", siteID).Where("workflow_id = ?", workflowID).Exec(ctx)
	return err
}

// DeleteAllBySiteID removes recovery records during Site teardown.
func (d *PendingExpectedMachineBatchSQLDAO) DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error {
	_, err := db.GetIDB(tx, d.dbSession).NewDelete().Model((*PendingExpectedMachine)(nil)).Where("site_id = ?", siteID).Exec(ctx)
	return err
}

// Complete materializes a confirmed batch and clears its reservation in tx.
// The caller holds the exclusive Site inventory lock in tx. Existing inventory or
// API values win; only missing request metadata and the inventory creator are repaired.
// Advancing Updated fences inventory that read an imported row before completion.
func (d *PendingExpectedMachineBatchSQLDAO) Complete(ctx context.Context, tx *db.Tx, siteID uuid.UUID, workflowID string) ([]ExpectedMachine, error) {
	_, err := NewSiteDAO(d.dbSession).GetByID(ctx, tx, siteID, nil, false, true)
	if err != nil {
		return nil, err
	}
	var rows []PendingExpectedMachine
	err = db.GetIDB(tx, d.dbSession).NewSelect().Model(&rows).Where("site_id = ?", siteID).Where("workflow_id = ?", workflowID).Order("position").Scan(ctx)
	if err != nil {
		return nil, err
	}
	if len(rows) == 0 {
		return nil, nil
	}
	dao := NewExpectedMachineDAO(d.dbSession)
	machines := make([]ExpectedMachine, 0, len(rows))
	for _, row := range rows {
		input := row.Input
		machine, err := dao.Get(ctx, tx, input.ExpectedMachineID, nil, true)
		if errors.Is(err, db.ErrDoesNotExist) {
			machine, err = dao.Create(ctx, tx, input)
		} else if err == nil {
			if machine.SiteID != siteID {
				return nil, errors.New("reserved Expected Machine belongs to another Site")
			}
			labelsJSON, marshalErr := json.Marshal(input.Labels)
			if marshalErr != nil {
				return nil, marshalErr
			}
			_, err = db.GetIDB(tx, d.dbSession).NewUpdate().Model(machine).
				Set("labels = CASE WHEN labels IS NULL OR labels = 'null'::jsonb THEN ?::jsonb ELSE labels END", string(labelsJSON)).
				Set("name = COALESCE(name, ?)", input.Name).
				Set("description = COALESCE(description, ?)", input.Description).
				Set("manufacturer = COALESCE(manufacturer, ?)", input.Manufacturer).
				Set("model = COALESCE(model, ?)", input.Model).
				Set("rack_id = COALESCE(rack_id, ?)", input.RackID).
				Set("server_name = COALESCE(server_name, ?)", input.ServerName).
				Set("slot_id = COALESCE(slot_id, ?)", input.SlotID).
				Set("tray_idx = COALESCE(tray_idx, ?)", input.TrayIdx).
				Set("host_id = COALESCE(host_id, ?)", input.HostID).
				Set("created_by = CASE WHEN created_by = ? THEN ? ELSE created_by END", siteID, input.CreatedBy).
				Set("updated = ?", db.GetCurTime()).
				Where("id = ?", input.ExpectedMachineID).Exec(ctx)
			if err == nil {
				machine, err = dao.Get(ctx, tx, input.ExpectedMachineID, nil, false)
			}
		}
		if err != nil {
			return nil, err
		}
		machines = append(machines, *machine)
	}
	err = d.Delete(ctx, tx, siteID, workflowID)
	return machines, err
}
