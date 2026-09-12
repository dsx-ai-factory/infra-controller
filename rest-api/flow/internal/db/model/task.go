// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

var defaultTaskPagination = dbquery.Pagination{
	Offset: 0,
	Limit:  100,
	Total:  0,
}

var defaultTaskOrderBy = []dbquery.OrderBy{
	{Column: "created_at", Direction: dbquery.OrderDescending},
	{Column: "id", Direction: dbquery.OrderDescending},
}

// Task models the persisted task metadata managed by Flow.
type Task struct {
	bun.BaseModel `bun:"table:task,alias:t"`

	ID            uuid.UUID                 `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Type          taskcommon.TaskType       `bun:"type,type:varchar(64),notnull"`
	ExecutorType  taskcommon.ExecutorType   `bun:"executor_type,type:varchar(64),nullzero"`
	Information   json.RawMessage           `bun:"information,type:jsonb,json_use_number"`
	Description   string                    `bun:"description,nullzero"`
	RackID        uuid.UUID                 `bun:"rack_id,type:uuid,notnull"` // The rack this task operates on
	Attributes    taskcommon.TaskAttributes `bun:"attributes,type:jsonb"`
	ExecutionID   string                    `bun:"execution_id,notnull"`
	Status        taskcommon.TaskStatus     `bun:"status,type:varchar(32),notnull"`
	Message       string                    `bun:"message,nullzero"`
	Report        json.RawMessage           `bun:"report,type:jsonb"`
	AppliedRuleID *uuid.UUID                `bun:"applied_rule_id,type:uuid"` // Which operation rule was applied
	CreatedAt     time.Time                 `bun:"created_at,nullzero,notnull,default:current_timestamp"`
	UpdatedAt     time.Time                 `bun:"updated_at,nullzero,notnull,default:current_timestamp"`
	StartedAt     *time.Time                `bun:"started_at"`
	FinishedAt    *time.Time                `bun:"finished_at"`

	// QueueExpiresAt is set for pre-execution waits. After this time, the
	// Promoter or task manager terminates the task instead of executing it.
	QueueExpiresAt *time.Time `bun:"queue_expires_at"`

	IdempotencyKey string     `bun:"idempotency_key,nullzero"`
	TriggerType    string     `bun:"trigger_type,nullzero"`
	TriggerID      *uuid.UUID `bun:"trigger_id,type:uuid"`
}

func (t *Task) HasIdempotencyKey() bool {
	return t != nil && t.IdempotencyKey != ""
}

// Create inserts the task record into the backing store.
func (t *Task) Create(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewInsert().Model(t).Exec(ctx)
	return err
}

// CreateOrGetByIdempotencyKey inserts the task or returns the existing task
// carrying the same idempotency key.
func (t *Task) CreateOrGetByIdempotencyKey(
	ctx context.Context,
	idb bun.IDB,
) (*Task, bool, error) {
	if !t.HasIdempotencyKey() {
		return nil, false, fmt.Errorf("idempotency key is required")
	}

	// First try to insert the candidate task. The partial unique index on
	// idempotency_key lets Postgres arbitrate concurrent submissions for the
	// same logical request.
	result, err := idb.NewInsert().
		Model(t).
		On("CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING").
		Exec(ctx)
	if err != nil {
		return nil, false, err
	}

	rowsAffected, err := result.RowsAffected()
	if err != nil {
		return nil, false, err
	}

	// A non-zero row count means this call created the task and the caller can
	// continue with the candidate row it supplied.
	if rowsAffected > 0 {
		return t, true, nil
	}

	// No row was inserted, so another attempt already created the task. Return
	// the persisted row so callers can recover the task ID and scheduling state.
	existing, err := GetTaskByIdempotencyKey(ctx, idb, t.IdempotencyKey)
	if err != nil {
		return nil, false, err
	}

	return existing, false, nil
}

// UpdateScheduledTask updates the scheduled task information.
func (t *Task) UpdateScheduledTask(
	ctx context.Context,
	idb bun.IDB,
) error {
	if t.ExecutionID == "" {
		return fmt.Errorf("execution ID is not set")
	}

	if t.ExecutorType == taskcommon.ExecutorTypeUnknown {
		return fmt.Errorf("executor type is not set")
	}

	t.UpdatedAt = time.Now().UTC()

	_, err := idb.NewUpdate().
		Model(t).
		Column("execution_id", "executor_type", "updated_at").
		Where("id = ?", t.ID).
		Exec(ctx)

	return err
}

// UpdateTaskStatus updates the status of the task. Non-nil report and
// queueExpiresAt values replace their corresponding stored columns. Finished
// statuses clear queueExpiresAt because the deadline applies only while waiting.
func (t *Task) UpdateTaskStatus(
	ctx context.Context,
	idb bun.IDB,
	status taskcommon.TaskStatus,
	message string,
	report json.RawMessage,
	queueExpiresAt *time.Time,
) error {
	t.Status = status
	t.Message = message
	t.UpdatedAt = time.Now().UTC()

	columns := []string{"status", "message", "updated_at", "finished_at"}
	if report != nil {
		t.Report = report
		columns = append(columns, "report")
	}
	if status == taskcommon.TaskStatusRunning && t.StartedAt == nil {
		t.StartedAt = &t.UpdatedAt
		columns = append(columns, "started_at")
	}
	if status.IsFinished() {
		t.FinishedAt = &t.UpdatedAt
		t.QueueExpiresAt = nil
		columns = append(columns, "queue_expires_at")
	} else {
		t.FinishedAt = nil
		if queueExpiresAt != nil {
			t.QueueExpiresAt = queueExpiresAt
			columns = append(columns, "queue_expires_at")
		}
	}

	_, err := idb.NewUpdate().
		Model(t).
		Column(columns...).
		Where("id = ?", t.ID).
		Exec(ctx)

	return err
}

// UpdateTaskReport updates the structured report without changing status or message.
func (t *Task) UpdateTaskReport(
	ctx context.Context,
	idb bun.IDB,
	report json.RawMessage,
) error {
	t.Report = report
	t.UpdatedAt = time.Now().UTC()

	_, err := idb.NewUpdate().
		Model(t).
		Column("report", "updated_at").
		Where("id = ?", t.ID).
		Exec(ctx)

	return err
}

// taskComponentIDFilter restricts a query to tasks whose attributes
// reference a specific component UUID. The UUID may live under any bucket
// of attributes.components_by_type, which rules out a fixed @> check
// against a known key and forces a jsonpath expression. The predicate is
// served by the GIN (jsonb_path_ops) index on task.attributes added in
// migration 20260517000000.
type taskComponentIDFilter struct {
	componentID uuid.UUID
}

func (f taskComponentIDFilter) ApplyTo(q *bun.SelectQuery) *bun.SelectQuery {
	// bun rewrites '?' in a query string as a placeholder, which collides
	// both with PG's '@?' operator and with jsonpath's own '?(filter)'
	// syntax. Build the predicate as raw SQL and inject it via bun.Safe so
	// bun does not rescan it for '?'. uuid.UUID.String() emits only hex
	// and hyphens, so it cannot close the SQL string literal or inject.
	expr := fmt.Sprintf(
		`attributes @? '$.components_by_type.*[*] ? (@ == "%s")'::jsonpath`,
		f.componentID.String(),
	)
	return q.Where("?", bun.Safe(expr))
}

func taskListOptionsToFilterable(
	options *taskcommon.TaskListOptions,
) dbquery.Filterable {
	if options == nil {
		return nil
	}

	filters := make([]dbquery.Filter, 0, 3)

	// Filter by rack_id directly
	if options.RackID != uuid.Nil {
		filters = append(
			filters,
			dbquery.Filter{
				Column:   "rack_id",
				Operator: dbquery.OperatorEqual,
				Value:    options.RackID,
			},
		)
	}

	if options.TaskType != taskcommon.TaskTypeUnknown {
		filters = append(filters, dbquery.Filter{
			Column:   "type",
			Operator: dbquery.OperatorEqual,
			Value:    options.TaskType,
		})
	}

	if options.ActiveOnly {
		filters = append(filters, dbquery.Filter{
			Column:   "status",
			Operator: dbquery.OperatorIn,
			Value:    taskcommon.NonTerminalTaskStatuses(),
		})
	}

	group := &dbquery.FilterGroup{
		Filters:   filters,
		Connector: dbquery.ConnectorAND,
	}

	// ComponentID needs a jsonpath expression on attributes, which the
	// generic Filter type does not express, so combine it with the rest
	// via a small composite Filterable.
	if options.ComponentID != uuid.Nil {
		return taskFilterables{
			group,
			taskComponentIDFilter{componentID: options.ComponentID},
		}
	}

	return group
}

// taskFilterables is an AND-combination of several Filterables. Each entry
// applies its own predicates to the same query, which bun joins with AND.
type taskFilterables []dbquery.Filterable

func (fs taskFilterables) ApplyTo(q *bun.SelectQuery) *bun.SelectQuery {
	for _, f := range fs {
		if f == nil {
			continue
		}
		q = f.ApplyTo(q)
	}
	return q
}

// GetTask retrieves the task by its UUID.
func GetTask(ctx context.Context, idb bun.IDB, id uuid.UUID) (*Task, error) {
	if id == uuid.Nil {
		return nil, fmt.Errorf("task UUID is required")
	}

	var task Task
	if err := idb.NewSelect().
		Model(&task).
		Where("id = ?", id).
		Scan(ctx); err != nil {
		return nil, err
	}
	return &task, nil
}

// GetTaskByIdempotencyKey retrieves one task by its stable submission key.
func GetTaskByIdempotencyKey(
	ctx context.Context,
	idb bun.IDB,
	key string,
) (*Task, error) {
	if key == "" {
		return nil, fmt.Errorf("idempotency key is required")
	}

	var task Task
	if err := idb.NewSelect().
		Model(&task).
		Where("idempotency_key = ?", key).
		Scan(ctx); err != nil {
		return nil, err
	}
	return &task, nil
}

// ListTasksForRackByStatus returns tasks for a rack matching any of the given
// statuses, ordered oldest-first.
func ListTasksForRackByStatus(
	ctx context.Context,
	idb bun.IDB,
	rackID uuid.UUID,
	statuses []taskcommon.TaskStatus,
) ([]Task, error) {
	var tasks []Task
	err := idb.NewSelect().
		Model(&tasks).
		Where("rack_id = ?", rackID).
		Where("status IN (?)", bun.In(statuses)).
		OrderExpr("created_at ASC").
		Scan(ctx)
	return tasks, err
}

// ListTasksForRacksByStatus returns tasks for the requested racks matching any
// of the given statuses.
func ListTasksForRacksByStatus(
	ctx context.Context,
	idb bun.IDB,
	rackIDs []uuid.UUID,
	statuses []taskcommon.TaskStatus,
) ([]Task, error) {
	if len(rackIDs) == 0 || len(statuses) == 0 {
		return []Task{}, nil
	}

	var tasks []Task
	err := listTasksForRacksByStatusQuery(idb, &tasks, rackIDs, statuses).
		Scan(ctx)
	return tasks, err
}

func listTasksForRacksByStatusQuery(
	idb bun.IDB,
	tasks *[]Task,
	rackIDs []uuid.UUID,
	statuses []taskcommon.TaskStatus,
) *bun.SelectQuery {
	return idb.NewSelect().
		Model(tasks).
		Column("id", "rack_id", "attributes", "status").
		Where("rack_id IN (?)", bun.In(rackIDs)).
		Where("status IN (?)", bun.In(statuses))
}

// ListRacksWithWaitingTasks returns the distinct rack IDs that have at least
// one task in the waiting state.
func ListRacksWithWaitingTasks(
	ctx context.Context,
	idb bun.IDB,
) ([]uuid.UUID, error) {
	var rackIDs []uuid.UUID
	err := idb.NewSelect().
		TableExpr("task").
		ColumnExpr("DISTINCT rack_id").
		Where("status = ?", taskcommon.TaskStatusWaiting).
		Scan(ctx, &rackIDs)
	return rackIDs, err
}

// CountWaitingTasksForRack returns the count of waiting tasks for a rack.
func CountWaitingTasksForRack(
	ctx context.Context,
	idb bun.IDB,
	rackID uuid.UUID,
) (int, error) {
	return idb.NewSelect().
		TableExpr("task").
		Where("rack_id = ?", rackID).
		Where("status = ?", taskcommon.TaskStatusWaiting).
		Count(ctx)
}

// ListTasks returns all tasks that match the given criteria.
func ListTasks(
	ctx context.Context,
	idb bun.IDB,
	options *taskcommon.TaskListOptions,
	pagination *dbquery.Pagination,
) ([]Task, int32, error) {
	var tasks []Task
	conf := &dbquery.Config{
		IDB:            idb,
		Model:          &tasks,
		DefaultOrderBy: defaultTaskOrderBy,
	}

	if pagination != nil {
		conf.Pagination = pagination
	} else {
		conf.Pagination = &defaultTaskPagination
	}

	if filterable := taskListOptionsToFilterable(options); filterable != nil {
		conf.Filterables = []dbquery.Filterable{filterable}
	}

	q, err := dbquery.New(ctx, conf)
	if err != nil {
		return nil, 0, err
	}

	if err := q.Scan(ctx); err != nil {
		return nil, 0, err
	}

	return tasks, int32(q.TotalCount()), nil
}
