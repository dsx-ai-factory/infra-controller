// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package taskschedule

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
)

// ListOptions filters results from List.
type ListOptions struct {
	// RackIDs, when non-empty, restricts results to schedules whose scope
	// includes at least one of the given racks.
	RackIDs []uuid.UUID
	// EnabledOnly, when true, excludes paused schedules from results.
	EnabledOnly bool
	// Pagination, when non-nil, applies offset/limit to the result set.
	Pagination *dbquery.Pagination
}

// UpdateFields carries the mutable fields for Update.
// Only non-zero fields are applied; zero values are left unchanged in the DB.
type UpdateFields struct {
	// Name, when non-empty, replaces the schedule's display name.
	Name string
	// SpecType, when non-empty, replaces the scheduling mechanism (interval/cron/one-time).
	SpecType dbmodel.SpecType
	// Spec, when non-empty, replaces the raw spec string (duration, cron expression, or RFC3339 timestamp).
	Spec string
	// Timezone, when non-empty, replaces the IANA timezone used to interpret cron specs.
	Timezone string
	// OverlapPolicy, when non-empty, replaces how the schedule behaves when the previous run is still active.
	OverlapPolicy dbmodel.OverlapPolicy
	// NextRunAt, when non-nil, replaces the next scheduled firing time.
	// Typically set together with Spec/SpecType when the spec changes.
	NextRunAt *time.Time
}

// AfterFireUpdate carries the schedule-level fields written back after a firing.
// Per-scope last_task_id updates are handled separately via UpdateScopeLastTaskIDs.
type AfterFireUpdate struct {
	// LastRunAt is the wall-clock time at which the schedule fired.
	LastRunAt time.Time
	// NextRunAt is the next scheduled firing time. nil sets next_run_at to NULL,
	// which marks a one-time schedule as fully consumed.
	NextRunAt *time.Time
	// Enabled reflects whether the schedule should remain active after this firing.
	// Set to false for one-time schedules after they fire.
	Enabled bool
}

// Store is the persistence layer for task schedules and their scopes.
//
// Methods that must run inside a transaction are documented with
// "Must be called within a RunInTransaction block." Calling them outside a
// transaction is safe but will use auto-commit semantics.
type Store interface {
	// Create inserts a new TaskSchedule row and returns the DB-generated ID.
	Create(ctx context.Context, ts *dbmodel.TaskSchedule) (uuid.UUID, error)

	// Get returns the TaskSchedule with the given ID, or an error if not found.
	Get(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error)

	// List returns schedules matching opts, along with the total count before
	// pagination is applied (useful for building paginated API responses).
	List(ctx context.Context, opts ListOptions) ([]*dbmodel.TaskSchedule, int32, error)

	// Update applies non-zero fields from UpdateFields to the schedule and
	// returns the updated row.
	Update(ctx context.Context, id uuid.UUID, fields UpdateFields) (*dbmodel.TaskSchedule, error)

	// SetEnabled sets the enabled flag on a schedule and returns the updated row.
	// Use this for pause operations rather than Update to make intent clear.
	SetEnabled(ctx context.Context, id uuid.UUID, enabled bool) (*dbmodel.TaskSchedule, error)

	// Resume sets enabled=true and, for interval/cron schedules, atomically
	// updates next_run_at in the same statement. Pass nil nextRunAt for
	// one-time schedules that were paused before firing (next_run_at unchanged).
	Resume(ctx context.Context, id uuid.UUID, nextRunAt *time.Time) (*dbmodel.TaskSchedule, error)

	// Delete permanently removes the schedule row. Associated scope rows are
	// removed automatically via ON DELETE CASCADE on the FK constraint.
	// Returns an error if no row with the given ID exists.
	Delete(ctx context.Context, id uuid.UUID) error

	// RunInTransaction executes fn within a database transaction. The transaction
	// is propagated through ctx so nested Store calls participate in it automatically.
	// fn must use the ctx it receives, not the outer ctx.
	RunInTransaction(ctx context.Context, fn func(ctx context.Context) error) error

	// FetchDue returns the IDs of up to batchSize enabled schedules whose
	// next_run_at is at or before now, ordered by next_run_at ascending.
	// This is a read-only scan with no row locking; use LockForFire inside a
	// transaction to claim individual rows before firing.
	FetchDue(ctx context.Context, batchSize int, now time.Time) ([]uuid.UUID, error)

	// LockForFire attempts SELECT ... FOR UPDATE SKIP LOCKED on the schedule row.
	// Returns the locked row if it is still enabled and due at now.
	// Returns nil (no error) if the row was already locked by another instance
	// or is no longer due (e.g. another instance already advanced it).
	// Must be called within a RunInTransaction block.
	LockForFire(ctx context.Context, id uuid.UUID, now time.Time) (*dbmodel.TaskSchedule, error)

	// LockForTrigger acquires a blocking SELECT ... FOR UPDATE lock on the row.
	// Unlike LockForFire it does not filter by enabled or next_run_at, and it
	// blocks (no SKIP LOCKED) so concurrent FireNow calls and a racing poller
	// all serialize against the same row lock. Returns an error if the row does
	// not exist.
	// Must be called within a RunInTransaction block.
	LockForTrigger(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error)

	// UpdateAfterFire writes back last_run_at, next_run_at, and enabled
	// after a successful firing. next_run_at is set to NULL when AfterFireUpdate.NextRunAt
	// is nil, which marks a one-time schedule as fully consumed.
	// Must be called within a RunInTransaction block.
	UpdateAfterFire(ctx context.Context, id uuid.UUID, u AfterFireUpdate) error

	// CreateScopes bulk-inserts scope rows. The ScheduleID field must be set on
	// each row before calling. A no-op if scopes is empty.
	CreateScopes(ctx context.Context, scopes []*dbmodel.TaskScheduleScope) error

	// ListScopes returns all scope rows for the given schedule, ordered by created_at ascending.
	// Returns an error if no schedule with the given ID exists.
	ListScopes(ctx context.Context, scheduleID uuid.UUID) ([]*dbmodel.TaskScheduleScope, error)

	// GetScope fetches a single scope row by its ID.
	// Returns an error if no row with the given ID exists.
	GetScope(ctx context.Context, scopeID uuid.UUID) (*dbmodel.TaskScheduleScope, error)

	// DeleteScope removes a single scope row by its ID.
	// Returns an error if no row with the given ID exists.
	DeleteScope(ctx context.Context, scopeID uuid.UUID) error

	// UpdateScopeLastTaskIDs sets last_task_id on each scope row in the map.
	// Keyed by scope ID; the value is the task ID produced by the most recent firing.
	// Must be called within a RunInTransaction block.
	UpdateScopeLastTaskIDs(ctx context.Context, scopeTaskIDs map[uuid.UUID]uuid.UUID) error

	// UpdateScopeComponentFilter replaces the component_filter JSONB on a scope row.
	// componentFilter must be nil or a value produced by dbmodel.MarshalComponentFilter;
	// passing arbitrary JSON bypasses the kind/field validation enforced by that function.
	// Pass nil to clear the filter, which causes the dispatcher to target all
	// components in the rack.
	UpdateScopeComponentFilter(ctx context.Context, scopeID uuid.UUID, componentFilter json.RawMessage) error
}

// txKeyType is an unexported type for the transaction context key, preventing
// accidental collisions with keys from other packages.
type txKeyType struct{}

var txKey = txKeyType{}

// PostgresStore implements Store using PostgreSQL via bun.
type PostgresStore struct {
	pg *cdb.Session
}

// NewPostgresStore creates a new PostgreSQL-backed task schedule store.
func NewPostgresStore(pg *cdb.Session) *PostgresStore {
	return &PostgresStore{pg: pg}
}

// idb returns the active bun.IDB for ctx: the bun.Tx if inside a
// RunInTransaction call, or the underlying *bun.DB otherwise.
func (s *PostgresStore) idb(ctx context.Context) bun.IDB {
	if tx, ok := ctx.Value(txKey).(bun.Tx); ok {
		return tx
	}
	return s.pg.DB
}

// RunInTransaction implements Store.
func (s *PostgresStore) RunInTransaction(
	ctx context.Context,
	fn func(ctx context.Context) error,
) error {
	return s.pg.DB.RunInTx(
		ctx,
		&sql.TxOptions{},
		func(ctx context.Context, tx bun.Tx) error { //nolint:exhaustruct,wrapcheck
			return fn(context.WithValue(ctx, txKey, tx))
		},
	)
}

// Create implements Store.
func (s *PostgresStore) Create(
	ctx context.Context,
	ts *dbmodel.TaskSchedule,
) (uuid.UUID, error) {
	var id uuid.UUID
	err := s.idb(ctx).NewInsert().Model(ts).Returning("id").Scan(ctx, &id)
	return id, err
}

// Get implements Store.
func (s *PostgresStore) Get(
	ctx context.Context,
	id uuid.UUID,
) (*dbmodel.TaskSchedule, error) {
	var ts dbmodel.TaskSchedule

	err := s.idb(ctx).NewSelect().
		Model(&ts).
		Where("ts.id = ?", id).
		Scan(ctx)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, fmt.Errorf("task schedule %s not found", id)
		}

		return nil, err
	}

	return &ts, nil
}

// List implements Store.
func (s *PostgresStore) List(
	ctx context.Context,
	opts ListOptions,
) ([]*dbmodel.TaskSchedule, int32, error) {
	var rows []dbmodel.TaskSchedule

	// Build the base query with filters only — no ORDER BY, no pagination.
	q := s.idb(ctx).NewSelect().Model(&rows)

	if len(opts.RackIDs) > 0 {
		q = q.Where("EXISTS (?)",
			s.idb(ctx).NewSelect().
				TableExpr("task_schedule_scope AS tss").
				ColumnExpr("1").
				Where("tss.schedule_id = ts.id").
				Where("tss.rack_id IN (?)", bun.In(opts.RackIDs)),
		)
	}

	if opts.EnabledOnly {
		q = q.Where("ts.enabled = TRUE")
	}

	// Count before applying pagination. An alternative is COUNT(*) OVER() in a
	// single query, but that window function returns 0 once OFFSET skips past
	// all matching rows, so out-of-range pages report total=0. A separate count
	// query is always correct at the cost of one extra round-trip; for an
	// admin-facing list that cost is negligible.
	total, err := q.Count(ctx)
	if err != nil {
		return nil, 0, err
	}
	if total == 0 {
		return nil, 0, nil
	}

	// Fetch the page.
	q = q.OrderExpr("ts.created_at ASC, ts.id ASC")
	if opts.Pagination != nil {
		q = q.Offset(opts.Pagination.Offset).Limit(opts.Pagination.Limit)
	}

	if err := q.Scan(ctx); err != nil {
		return nil, 0, err
	}

	schedules := make([]*dbmodel.TaskSchedule, len(rows))
	for i := range rows {
		ts := rows[i]
		schedules[i] = &ts
	}

	return schedules, int32(total), nil
}

// Update implements Store.
func (s *PostgresStore) Update(
	ctx context.Context,
	id uuid.UUID,
	fields UpdateFields,
) (*dbmodel.TaskSchedule, error) {
	q := s.idb(ctx).NewUpdate().
		TableExpr("task_schedule").
		Where("id = ?", id)

	count := 0
	if fields.Name != "" {
		q = q.Set("name = ?", fields.Name)
		count++
	}

	if fields.SpecType != "" {
		q = q.Set("spec_type = ?", fields.SpecType)
		count++
	}

	if fields.Spec != "" {
		q = q.Set("spec = ?", fields.Spec)
		count++
	}

	if fields.Timezone != "" {
		q = q.Set("timezone = ?", fields.Timezone)
		count++
	}

	if fields.OverlapPolicy != "" {
		q = q.Set("overlap_policy = ?", fields.OverlapPolicy)
		count++
	}

	if fields.NextRunAt != nil {
		q = q.Set("next_run_at = ?", *fields.NextRunAt)
		count++
	}

	// buildUpdateFields can zero out a field (e.g. timezone for non-cron schedules)
	// leaving nothing to SET. Calling bun's NewUpdate().TableExpr(...) with no
	// .Set() causes "bun: Model(nil)", so skip the DB round-trip when no fields changed.
	if count > 0 {
		if _, err := q.Exec(ctx); err != nil {
			return nil, err
		}
	}

	return s.Get(ctx, id)
}

// SetEnabled implements Store.
func (s *PostgresStore) SetEnabled(
	ctx context.Context, id uuid.UUID, enabled bool,
) (*dbmodel.TaskSchedule, error) {
	if _, err := s.idb(ctx).NewUpdate().
		TableExpr("task_schedule").
		Set("enabled = ?", enabled).
		Where("id = ?", id).
		Exec(ctx); err != nil {
		return nil, err
	}

	return s.Get(ctx, id)
}

// Resume implements Store.
func (s *PostgresStore) Resume(
	ctx context.Context, id uuid.UUID, nextRunAt *time.Time,
) (*dbmodel.TaskSchedule, error) {
	q := s.idb(ctx).NewUpdate().
		TableExpr("task_schedule").
		Set("enabled = TRUE").
		Where("id = ?", id)

	if nextRunAt != nil {
		q = q.Set("next_run_at = ?", *nextRunAt)
	}

	if _, err := q.Exec(ctx); err != nil {
		return nil, err
	}

	return s.Get(ctx, id)
}

// Delete implements Store.
func (s *PostgresStore) Delete(ctx context.Context, id uuid.UUID) error {
	res, err := s.idb(ctx).NewDelete().
		TableExpr("task_schedule").
		Where("id = ?", id).
		Exec(ctx)
	if err != nil {
		return err
	}

	n, err := res.RowsAffected()
	if err != nil {
		return err
	}

	if n == 0 {
		return fmt.Errorf("task schedule %s not found", id)
	}

	return nil
}

// FetchDue implements Store.
func (s *PostgresStore) FetchDue(
	ctx context.Context, batchSize int, now time.Time,
) ([]uuid.UUID, error) {
	var ids []uuid.UUID
	err := s.idb(ctx).NewSelect().
		TableExpr("task_schedule AS ts").
		ColumnExpr("ts.id").
		Where("ts.enabled = TRUE").
		Where("ts.next_run_at <= ?", now).
		OrderExpr("ts.next_run_at ASC").
		Limit(batchSize).
		Scan(ctx, &ids)

	return ids, err
}

// LockForFire implements Store.
func (s *PostgresStore) LockForFire(
	ctx context.Context,
	id uuid.UUID,
	now time.Time,
) (*dbmodel.TaskSchedule, error) {
	var schedules []*dbmodel.TaskSchedule
	err := s.idb(ctx).NewSelect().
		Model(&schedules).
		Where("ts.id = ?", id).
		Where("ts.enabled = TRUE").
		Where("ts.next_run_at <= ?", now).
		For("UPDATE SKIP LOCKED").
		Scan(ctx)
	if err != nil {
		return nil, err
	}

	if len(schedules) == 0 {
		return nil, nil // locked by another instance or no longer due
	}

	return schedules[0], nil
}

// LockForTrigger implements Store.
func (s *PostgresStore) LockForTrigger(
	ctx context.Context,
	id uuid.UUID,
) (*dbmodel.TaskSchedule, error) {
	var schedules []*dbmodel.TaskSchedule
	err := s.idb(ctx).NewSelect().
		Model(&schedules).
		Where("ts.id = ?", id).
		For("UPDATE"). // blocking — serializes concurrent triggers and the poller
		Scan(ctx)
	if err != nil {
		return nil, err
	}

	if len(schedules) == 0 {
		return nil, fmt.Errorf("task schedule %s not found", id)
	}

	return schedules[0], nil
}

// UpdateAfterFire implements Store.
func (s *PostgresStore) UpdateAfterFire(
	ctx context.Context,
	id uuid.UUID,
	u AfterFireUpdate,
) error {
	q := s.idb(ctx).NewUpdate().
		TableExpr("task_schedule").
		Set("last_run_at = ?", u.LastRunAt).
		Set("enabled = ?", u.Enabled).
		Where("id = ?", id)

	if u.NextRunAt != nil {
		q = q.Set("next_run_at = ?", *u.NextRunAt)
	} else {
		q = q.Set("next_run_at = NULL")
	}

	_, err := q.Exec(ctx)
	return err
}

// ─── Scope management ────────────────────────────────────────────────────────

// CreateScopes implements Store.
func (s *PostgresStore) CreateScopes(
	ctx context.Context,
	scopes []*dbmodel.TaskScheduleScope,
) error {
	if len(scopes) == 0 {
		return nil
	}
	_, err := s.idb(ctx).NewInsert().Model(&scopes).Exec(ctx)
	return err
}

// ListScopes implements Store.
func (s *PostgresStore) ListScopes(
	ctx context.Context,
	scheduleID uuid.UUID,
) ([]*dbmodel.TaskScheduleScope, error) {
	var scopes []*dbmodel.TaskScheduleScope
	err := s.idb(ctx).NewSelect().
		Model(&scopes).
		Where("tss.schedule_id = ?", scheduleID).
		OrderExpr("tss.created_at ASC").
		Scan(ctx)
	if err != nil {
		return nil, err
	}

	if len(scopes) == 0 {
		// An empty result is ambiguous: it can mean the schedule does not exist
		// OR that it exists but has no scope rows (an invariant violation that
		// the create path prevents). Probe the parent row so callers get a
		// clear not-found error instead of a silent empty list for an invalid ID.
		if _, err := s.Get(ctx, scheduleID); err != nil {
			return nil, err
		}
	}

	return scopes, nil
}

// GetScope implements Store.
func (s *PostgresStore) GetScope(
	ctx context.Context,
	scopeID uuid.UUID,
) (*dbmodel.TaskScheduleScope, error) {
	var scope dbmodel.TaskScheduleScope
	err := s.idb(ctx).NewSelect().
		Model(&scope).
		Where("tss.id = ?", scopeID).
		Scan(ctx)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, fmt.Errorf("task schedule scope %s not found", scopeID)
		}
		return nil, err
	}

	return &scope, nil
}

// DeleteScope implements Store.
func (s *PostgresStore) DeleteScope(
	ctx context.Context,
	scopeID uuid.UUID,
) error {
	res, err := s.idb(ctx).NewDelete().
		TableExpr("task_schedule_scope").
		Where("id = ?", scopeID).
		Exec(ctx)
	if err != nil {
		return err
	}

	n, err := res.RowsAffected()
	if err != nil {
		return err
	}

	if n == 0 {
		return fmt.Errorf("task schedule scope %s not found", scopeID)
	}

	return nil
}

// UpdateScopeComponentFilter implements Store.
func (s *PostgresStore) UpdateScopeComponentFilter(
	ctx context.Context,
	scopeID uuid.UUID,
	componentFilter json.RawMessage,
) error {
	// Validate before writing: UnmarshalComponentFilter parses and runs the
	// same kind/field checks as MarshalComponentFilter, so malformed JSONB
	// is caught here regardless of whether the caller validated upstream.
	if _, err := dbmodel.UnmarshalComponentFilter(componentFilter); err != nil {
		return fmt.Errorf(
			"invalid component_filter for scope %s: %w", scopeID, err,
		)
	}

	scope := &dbmodel.TaskScheduleScope{
		ID:              scopeID,
		ComponentFilter: componentFilter,
	}
	_, err := s.idb(ctx).NewUpdate().
		Model(scope).
		Column("component_filter").
		WherePK().
		Exec(ctx)

	return err
}

// UpdateScopeLastTaskIDs implements Store.
func (s *PostgresStore) UpdateScopeLastTaskIDs(
	ctx context.Context,
	scopeTaskIDs map[uuid.UUID]uuid.UUID,
) error {
	if len(scopeTaskIDs) == 0 {
		return nil
	}

	// Single UPDATE ... FROM (VALUES ...) instead of one round trip per scope.
	// Explicit ::uuid casts are required because PostgreSQL infers VALUES columns
	// as text when the parameters are sent as strings.
	placeholders := make([]string, 0, len(scopeTaskIDs))
	args := make([]any, 0, len(scopeTaskIDs)*2)
	for scopeID, taskID := range scopeTaskIDs {
		placeholders = append(placeholders, "(?::uuid, ?::uuid)")
		args = append(args, scopeID, taskID)
	}

	_, err := s.idb(ctx).NewRaw(
		"UPDATE task_schedule_scope AS tss"+
			" SET last_task_id = v.task_id"+
			" FROM (VALUES "+strings.Join(placeholders, ", ")+") AS v(scope_id, task_id)"+
			" WHERE tss.id = v.scope_id",
		args...,
	).Exec(ctx)

	return err
}
