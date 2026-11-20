// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
)

type eventHandlingRow struct {
	AppliedRuleID   uuid.UUID              `bun:"applied_rule_id"`
	ID              uuid.UUID              `bun:"id"`
	EventType       eventrule.Type         `bun:"event_type"`
	EventSummary    string                 `bun:"summary"`
	FirstObservedAt *time.Time             `bun:"first_observed_at"`
	EffectivePolicy json.RawMessage        `bun:"effective_policy"`
	ResourceID      string                 `bun:"resource_id"`
	ResourceType    eventrule.ResourceKind `bun:"resource_type"`
	HandledAt       time.Time              `bun:"handled_at"`
}

type eventHandlingExecutionRow struct {
	ID         uuid.UUID                  `bun:"id"`
	EventID    uuid.UUID                  `bun:"event_id"`
	ActionName string                     `bun:"action_name"`
	Status     eventrule.ExecutionStatus  `bun:"status"`
	Reason     *eventrule.ExecutionReason `bun:"reason"`
}

type eventHandlingTaskRow struct {
	EventID     uuid.UUID             `bun:"event_id"`
	ExecutionID uuid.UUID             `bun:"execution_id"`
	TaskID      uuid.UUID             `bun:"task_id"`
	Status      taskcommon.TaskStatus `bun:"status"`
	CreatedAt   time.Time             `bun:"created_at"`
}

// ListEventHandlings applies optional filters to captured context before pagination.
func (s *PostgresStore) ListEventHandlings(ctx context.Context, filter eventrule.HandlingQuery) ([]eventrule.Handling, int, error) {
	rows := make([]eventHandlingRow, 0)
	query := filterEventContext(s.idb(ctx).NewSelect(), filter).
		TableExpr("events AS e").
		ColumnExpr("e.id").
		ColumnExpr("e.event_type").
		ColumnExpr("e.applied_rule_id").
		ColumnExpr("e.summary").
		ColumnExpr("e.first_observed_at").
		ColumnExpr("e.effective_policy").
		ColumnExpr("COALESCE(c.external_id, r.external_id, '') AS resource_id").
		ColumnExpr("e.resource_kind AS resource_type").
		ColumnExpr("e.created_at AS handled_at").
		Join("LEFT JOIN component AS c ON e.resource_kind = 'component' AND c.id = e.resource_id").
		Join("LEFT JOIN rack AS r ON e.resource_kind = 'rack' AND r.id = e.resource_id").
		OrderExpr("e.created_at DESC, e.id DESC").
		Offset(filter.Offset).
		Limit(filter.Limit)
	total, err := query.ScanAndCount(ctx, &rows)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal(err.Error())
	}

	handlings := make([]eventrule.Handling, 0, len(rows))
	handlingByID := make(map[uuid.UUID]int, len(rows))
	ids := make([]uuid.UUID, 0, len(rows))
	for _, row := range rows {
		policy, err := eventrulecodec.UnmarshalPolicy(row.EffectivePolicy)
		if err != nil {
			return nil, 0, errors.GRPCErrorInternal("decode applied event policy: " + err.Error())
		}
		handlingByID[row.ID] = len(handlings)
		ids = append(ids, row.ID)
		handlings = append(handlings, eventrule.Handling{
			ID:            row.ID,
			AppliedRuleID: row.AppliedRuleID,
			EventType:     row.EventType,
			EventSummary:  row.EventSummary,
			AppliedPolicy: policy,
			ResourceID:    row.ResourceID,
			ResourceType:  row.ResourceType,
			Executions:    make([]eventrule.HandlingExecution, 0),
			Tasks:         make([]eventrule.HandlingTask, 0),
			HandledAt:     row.HandledAt.UTC(),
		})
		if row.FirstObservedAt != nil {
			handlings[len(handlings)-1].FirstObservedAt = row.FirstObservedAt.UTC()
		}
	}
	if len(ids) == 0 {
		return handlings, total, nil
	}

	executionRows := make([]eventHandlingExecutionRow, 0)
	err = s.idb(ctx).NewSelect().
		TableExpr("event_action_executions AS eae").
		ColumnExpr("eae.id, eae.event_id, eae.action_name, eae.status, eae.reason").
		Where("eae.event_id IN (?)", bun.In(ids)).
		OrderExpr("eae.event_id, eae.action_name ASC, eae.id ASC").
		Scan(ctx, &executionRows)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal(err.Error())
	}
	for _, row := range executionRows {
		execution := eventrule.HandlingExecution{ID: row.ID, ActionName: row.ActionName, Status: row.Status}
		if row.Reason != nil {
			execution.Reason = *row.Reason
		}
		index := handlingByID[row.EventID]
		handlings[index].Executions = append(handlings[index].Executions, execution)
	}

	taskRows := make([]eventHandlingTaskRow, 0)
	err = s.idb(ctx).NewSelect().
		TableExpr("event_action_executions AS eae").
		ColumnExpr("eae.event_id").
		ColumnExpr("eae.id AS execution_id").
		ColumnExpr("t.id AS task_id").
		ColumnExpr("t.status").
		ColumnExpr("t.created_at").
		Join("JOIN task AS t ON t.trigger_id = eae.id").
		Where("eae.event_id IN (?)", bun.In(ids)).
		Where("t.trigger_type = ?", operation.TriggerTypeEventRuleExecution).
		OrderExpr("eae.event_id, t.created_at ASC, t.id ASC").
		Scan(ctx, &taskRows)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal(err.Error())
	}
	for _, row := range taskRows {
		index, ok := handlingByID[row.EventID]
		if ok {
			handlings[index].Tasks = append(handlings[index].Tasks, eventrule.HandlingTask{
				ID:          row.TaskID,
				ExecutionID: row.ExecutionID,
				Status:      row.Status,
				CreatedAt:   row.CreatedAt.UTC(),
			})
		}
	}

	return handlings, total, nil
}

func filterEventContext(query *bun.SelectQuery, filter eventrule.HandlingQuery) *bun.SelectQuery {
	if filter.EventType != "" {
		query = query.Where("e.event_type = ?", filter.EventType)
	}
	if filter.PolicyID != uuid.Nil {
		query = query.Where("e.applied_rule_id = ?", filter.PolicyID)
	}
	if filter.RackID != uuid.Nil {
		query = query.Where("e.rack_id = ?", filter.RackID)
	}
	if filter.ResourceType != "" {
		query = query.Where("e.resource_kind = ?", filter.ResourceType)
	}
	if filter.ResourceID != uuid.Nil {
		query = query.Where("e.resource_id = ?", filter.ResourceID)
	}
	return query
}

// ListEventTasks joins Task provenance to the immutable event associations.
// Task.applied_rule_id identifies an operation rule and must not be used here.
func (s *PostgresStore) ListEventTasks(ctx context.Context, filter eventrule.HandlingQuery, activeOnly bool) ([]*taskdef.Task, int32, error) {
	var records []dbmodel.Task
	query := filterEventContext(s.idb(ctx).NewSelect().Model(&records), filter).
		Join("JOIN event_action_executions AS eae ON eae.id = t.trigger_id").
		Join("JOIN events AS e ON e.id = eae.event_id").
		Where("t.trigger_type = ?", operation.TriggerTypeEventRuleExecution).
		OrderExpr("t.created_at DESC, t.id DESC").Offset(filter.Offset).Limit(filter.Limit)
	if activeOnly {
		query = query.Where("t.status IN (?)", bun.In([]taskcommon.TaskStatus{taskcommon.TaskStatusWaiting, taskcommon.TaskStatusPending, taskcommon.TaskStatusRunning}))
	}
	total, err := query.ScanAndCount(ctx)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal("Failed to list event Tasks")
	}
	result := make([]*taskdef.Task, 0, len(records))
	for i := range records {
		task, err := taskFromDAO(&records[i])
		if err != nil {
			return nil, 0, err
		}
		result = append(result, task)
	}
	return result, int32(total), nil
}
