// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// notifyingTaskStore wraps a taskstore.Store and fires Promoter notifications
// whenever a task transitions to a finished state. This bridges task completion
// events to the Promoter without coupling workflow activities to Manager
// internals.
type notifyingTaskStore struct {
	taskstore.Store
	promoter *conflict.Promoter
}

func newNotifyingTaskStore(
	store taskstore.Store,
	promoter *conflict.Promoter,
) *notifyingTaskStore {
	return &notifyingTaskStore{
		Store:    store,
		promoter: promoter,
	}
}

// UpdateTaskStatus delegates to the underlying store and notifies the Promoter
// when the task reaches a finished state. The task is fetched from the DB to
// obtain its rack ID, making this correct across service restarts.
func (s *notifyingTaskStore) UpdateTaskStatus(
	ctx context.Context,
	u *taskdef.TaskStatusUpdate,
) error {
	if err := s.Store.UpdateTaskStatus(ctx, u); err != nil {
		return err
	}
	if !u.Status.IsFinished() {
		return nil
	}

	// Delegate explicitly so a future wrapper override cannot change this
	// post-update lookup's semantics.
	tasks, err := s.Store.GetTasks(ctx, []uuid.UUID{u.ID}) //nolint:staticcheck // Intentional embedded-field qualification.
	if err != nil || len(tasks) == 0 {
		log.Warn().
			Str("task_id", u.ID.String()).
			Msg("notifying store: could not look up task for promotion")
		return nil
	}

	s.promoter.Notify(tasks[0].RackID)
	return nil
}

// UpdateTaskReport delegates to the underlying store.
func (s *notifyingTaskStore) UpdateTaskReport(
	ctx context.Context,
	u *taskdef.TaskReportUpdate,
) error {
	return s.Store.UpdateTaskReport(ctx, u)
}

// Ensure notifyingTaskStore satisfies taskdef.TaskStatusUpdater so it can
// be passed to activity.SetTaskStatusUpdater.
var _ taskdef.TaskStatusUpdater = (*notifyingTaskStore)(nil) //nolint

var _ taskdef.TaskReportUpdater = (*notifyingTaskStore)(nil) //nolint
