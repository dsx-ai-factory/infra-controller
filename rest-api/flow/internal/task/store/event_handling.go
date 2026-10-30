// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
)

type eventHandlingRow struct {
	ID           uuid.UUID              `bun:"id"`
	EventType    eventrule.Type         `bun:"event_type"`
	ResourceID   string                 `bun:"resource_id"`
	ResourceType eventrule.ResourceKind `bun:"resource_type"`
	HandledAt    time.Time              `bun:"handled_at"`
}

type eventHandlingTaskRow struct {
	EventID   uuid.UUID             `bun:"event_id"`
	TaskID    uuid.UUID             `bun:"task_id"`
	Status    taskcommon.TaskStatus `bun:"status"`
	CreatedAt time.Time             `bun:"created_at"`
}

// GetEventHandling returns one page of handling decisions for an event type
// and inventory resource, newest first. The boolean result reports whether
// another page exists.
func (s *PostgresStore) GetEventHandling(
	ctx context.Context,
	eventType eventrule.Type,
	resourceType eventrule.ResourceKind,
	resourceID uuid.UUID,
	limit int,
	cursor *eventrule.HandlingCursor,
) ([]eventrule.Handling, bool, error) {
	rows := make([]eventHandlingRow, 0)
	query := s.idb(ctx).NewSelect().
		TableExpr("events AS e").
		ColumnExpr("e.id").
		ColumnExpr("e.event_type").
		ColumnExpr("COALESCE(c.external_id, r.external_id, '') AS resource_id").
		ColumnExpr("e.resource_kind AS resource_type").
		ColumnExpr("e.created_at AS handled_at").
		Join("LEFT JOIN component AS c ON e.resource_kind = 'component' AND c.id = e.resource_id").
		Join("LEFT JOIN rack AS r ON e.resource_kind = 'rack' AND r.id = e.resource_id").
		Where("e.event_type = ?", eventType).
		Where("e.resource_kind = ?", resourceType).
		Where("e.resource_id = ?", resourceID).
		OrderExpr("e.created_at DESC, e.id DESC").
		Limit(limit + 1)
	if cursor != nil {
		query = query.Where("(e.created_at, e.id) < (?, ?)", cursor.HandledAt, cursor.ID)
	}
	if err := query.Scan(ctx, &rows); err != nil {
		return nil, false, errors.GRPCErrorInternal(err.Error())
	}
	hasMore := len(rows) > limit
	if hasMore {
		rows = rows[:limit]
	}

	handlings := make([]eventrule.Handling, 0, len(rows))
	handlingByID := make(map[uuid.UUID]int, len(rows))
	ids := make([]uuid.UUID, 0, len(rows))
	for _, row := range rows {
		handlingByID[row.ID] = len(handlings)
		ids = append(ids, row.ID)
		handlings = append(handlings, eventrule.Handling{
			ID:           row.ID,
			EventType:    row.EventType,
			ResourceID:   row.ResourceID,
			ResourceType: row.ResourceType,
			Tasks:        make([]eventrule.HandlingTask, 0),
			HandledAt:    row.HandledAt,
		})
	}
	if len(ids) == 0 {
		return handlings, hasMore, nil
	}

	taskRows := make([]eventHandlingTaskRow, 0)
	err := s.idb(ctx).NewSelect().
		TableExpr("event_action_executions AS eae").
		ColumnExpr("eae.event_id").
		ColumnExpr("t.id AS task_id").
		ColumnExpr("t.status").
		ColumnExpr("t.created_at").
		Join("JOIN task AS t ON t.trigger_id = eae.id").
		Where("eae.event_id IN (?)", bun.In(ids)).
		Where("t.trigger_type = ?", operation.TriggerTypeEventRuleExecution).
		OrderExpr("eae.event_id, t.created_at ASC, t.id ASC").
		Scan(ctx, &taskRows)
	if err != nil {
		return nil, false, errors.GRPCErrorInternal(err.Error())
	}
	for _, row := range taskRows {
		if index, ok := handlingByID[row.EventID]; ok {
			handlings[index].Tasks = append(handlings[index].Tasks, eventrule.HandlingTask{
				ID:        row.TaskID,
				Status:    row.Status,
				CreatedAt: row.CreatedAt,
			})
		}
	}

	return handlings, hasMore, nil
}
