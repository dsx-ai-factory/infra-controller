// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"
	"slices"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// createResetTasks admits every selected rack before scheduling any destructive
// work. Unlike best-effort bulk operations, a reset must not silently omit racks
// that conflict. Sorted rack locks avoid deadlocks between overlapping domains.
func (m *ManagerImpl) createResetTasks(ctx context.Context, req *operation.Request, racks map[uuid.UUID]*rack.Rack) ([]uuid.UUID, error) {
	ids := make([]uuid.UUID, 0, len(racks))
	for id := range racks {
		ids = append(ids, id)
	}
	slices.SortFunc(ids, func(a, b uuid.UUID) int { return slices.Compare(a[:], b[:]) })
	tasks := make([]taskdef.Task, 0, len(ids))
	err := m.taskStore.RunInTransaction(ctx, func(txCtx context.Context) error {
		for _, id := range ids {
			current := newTaskForRack(req, racks[id])
			err := m.lockRackAndResolveConflict(txCtx, req, racks[id], &current)
			if err != nil {
				return err
			}
			tasks = append(tasks, current)
		}
		for i := range tasks {
			err := m.taskStore.CreateTask(txCtx, &tasks[i])
			if err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	taskIDs := make([]uuid.UUID, 0, len(tasks))
	for i := range tasks {
		current := &tasks[i]
		taskIDs = append(taskIDs, current.ID)
		err := m.resolveAndExecuteTask(ctx, current, racks[current.RackID])
		if err != nil {
			// A task is durable already. Return its ID so callers can observe the
			// recorded scheduling failure rather than losing accepted peer tasks.
			log.Error().Err(err).Str("task_id", current.ID.String()).Msg("failed to schedule reset task")
		}
	}
	if len(taskIDs) == 0 {
		return nil, fmt.Errorf("no reset tasks created")
	}
	return taskIDs, nil
}
