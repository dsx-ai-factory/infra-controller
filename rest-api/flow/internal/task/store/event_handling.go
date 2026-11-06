// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
)

type eventHandlingRow struct {
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

// GetEventHandling returns one page of handling decisions for an event type
// and inventory resource, newest first. The count is the total matching history
// before applying the offset and limit.
func (s *PostgresStore) GetEventHandling(
	ctx context.Context,
	eventType eventrule.Type,
	resourceType eventrule.ResourceKind,
	resourceID uuid.UUID,
	offset int,
	limit int,
) ([]eventrule.Handling, int, error) {
	rows := make([]eventHandlingRow, 0)
	query := s.idb(ctx).NewSelect().
		TableExpr("events AS e").
		ColumnExpr("e.id").
		ColumnExpr("e.event_type").
		ColumnExpr("e.summary").
		ColumnExpr("e.first_observed_at").
		ColumnExpr("e.effective_policy").
		ColumnExpr("COALESCE(c.external_id, r.external_id, '') AS resource_id").
		ColumnExpr("e.resource_kind AS resource_type").
		ColumnExpr("e.created_at AS handled_at").
		Join("LEFT JOIN component AS c ON e.resource_kind = 'component' AND c.id = e.resource_id").
		Join("LEFT JOIN rack AS r ON e.resource_kind = 'rack' AND r.id = e.resource_id").
		Where("e.event_type = ?", eventType).
		Where("e.resource_kind = ?", resourceType).
		Where("e.resource_id = ?", resourceID).
		OrderExpr("e.created_at DESC, e.id DESC").
		Offset(offset).
		Limit(limit)
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
