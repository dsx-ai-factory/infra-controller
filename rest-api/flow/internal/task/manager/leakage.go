// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

func isDirectForcePowerOff(op operation.Wrapper) bool {
	if op.Type != taskcommon.TaskTypePowerControl {
		return false
	}
	var info operations.PowerControlTaskInfo
	return json.Unmarshal(op.Info, &info) == nil && info.Operation == operations.PowerOperationForcePowerOff && info.OverrideReadinessCheck && info.Validate() == nil && op.Code == info.CodeString()
}

// SubmitLeakageShutdown submits forced power-off work from a trusted leakage
// response. It terminates conflicting active and waiting tasks atomically with
// reserving the new task. Ordinary SubmitTask callers retain queue/reject behavior.
func (m *ManagerImpl) SubmitLeakageShutdown(ctx context.Context, req *operation.Request) ([]uuid.UUID, error) {
	if req == nil || !isDirectForcePowerOff(req.Operation) {
		return nil, fmt.Errorf("leakage shutdown requires force power off with readiness override")
	}
	return m.submitTask(ctx, req, m.createAndExecuteLeakageTask, false)
}

// Commit the displaced tasks' terminal state and the emergency reservation
// together, before contacting Temporal. A dispatch failure must not roll back
// the fence and allow an old readiness waiter to resume hardware operations.
func (m *ManagerImpl) createAndExecuteLeakageTask(ctx context.Context, req *operation.Request, targetRack *rack.Rack) (uuid.UUID, error) {
	task := newTaskForRack(req, targetRack)
	var displaced []*taskdef.Task
	existingReservation := false
	err := m.taskStore.RunInTransaction(ctx, func(txCtx context.Context) error {
		displaced = nil
		existingReservation = false
		if req.HasIdempotencyKey() {
			if err := m.taskStore.LockIdempotencyKey(txCtx, req.IdempotencyKey); err != nil {
				return err
			}
			existing, err := m.taskStore.GetTaskByIdempotencyKey(txCtx, req.IdempotencyKey)
			if err != nil {
				return err
			}
			if existing != nil {
				if err := validateIdempotentTaskRack(req, existing); err != nil {
					return err
				}
				task = *existing
				existingReservation = true
				if task.IsScheduled() || task.Status.IsFinished() {
					return nil
				}
			}
		}
		if err := m.taskStore.LockRack(txCtx, task.RackID); err != nil {
			return err
		}
		tasks, err := m.taskStore.ListActiveTasksForRack(txCtx, task.RackID)
		if err != nil {
			return err
		}
		waiting, err := m.taskStore.ListWaitingTasksForRack(txCtx, task.RackID)
		if err != nil {
			return err
		}
		tasks = append(tasks, waiting...)
		for _, existing := range tasks {
			// Direct forced shutdowns already have the same safe direction.
			// Leave them running rather than repeatedly terminating each other.
			if existing.ID == task.ID || isDirectForcePowerOff(existing.Operation) || !conflict.Conflicts(&task, existing) {
				continue
			}
			err := m.taskStore.UpdateTaskStatus(txCtx, &taskdef.TaskStatusUpdate{
				ID: existing.ID, Status: taskcommon.TaskStatusTerminated,
				Message: fmt.Sprintf("Preempted by emergency leakage shutdown %s; resubmit manually after the leak is cleared", task.ID),
			})
			if err != nil {
				return err
			}
			displaced = append(displaced, existing)
		}
		task.Status = taskcommon.TaskStatusPending
		task.Message = "Emergency leakage shutdown: bypassing the conflict queue"
		if existingReservation {
			return m.taskStore.UpdateTaskStatus(txCtx, &taskdef.TaskStatusUpdate{ID: task.ID, Status: task.Status, Message: task.Message})
		}
		return m.taskStore.CreateTask(txCtx, &task)
	})
	if err != nil {
		return uuid.Nil, err
	}
	// Termination is cleanup, not the dispatch fence. Run it after the emergency
	// submission so a slow/unavailable termination RPC cannot delay power-off.
	defer m.terminatePreempted(ctx, displaced)
	if req.HasIdempotencyKey() {
		return m.createAndExecuteIdempotentTask(ctx, req, targetRack)
	}
	if err := m.resolveAndExecuteTask(ctx, &task, targetRack); err != nil {
		return uuid.Nil, err
	}
	return task.ID, nil
}

func (m *ManagerImpl) terminatePreempted(ctx context.Context, tasks []*taskdef.Task) {
	cleanupCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer cancel()
	for _, task := range tasks {
		if !task.IsScheduled() {
			continue
		}
		if cleanupCtx.Err() != nil {
			return
		}
		err := m.executor.TerminateTask(cleanupCtx, task.ExecutionID, "Preempted by emergency leakage shutdown")
		if err != nil {
			log.Warn().Err(err).Str("task_id", task.ID.String()).Msg("Failed to terminate preempted workflow; persisted task fence remains active")
		}
	}
}
