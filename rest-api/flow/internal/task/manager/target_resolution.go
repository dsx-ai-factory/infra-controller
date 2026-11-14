// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/message"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	flowerrors "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

const (
	batchDispatchRecoveryInterval = 5 * time.Second
	batchDispatchRecoveryTimeout  = 30 * time.Second
)

// resolveSubmissionTargets runs after selector/type filtering. Only linkage
// failures are skippable; missing selectors, invalid rules and readiness remain errors.
func (m *ManagerImpl) resolveSubmissionTargets(ctx context.Context, req *operation.Request, racks map[uuid.UUID]*rack.Rack) (map[uuid.UUID]*taskcommon.TargetResolution, error) {
	results := make(map[uuid.UUID]*taskcommon.TargetResolution, len(racks))
	for id, selected := range racks {
		if !req.SkipUnaddressableTargets {
			if selected != nil {
				results[id] = &taskcommon.TargetResolution{SkipUnaddressableTargets: req.SkipUnaddressableTargets, Selected: len(selected.Components), Submitted: len(selected.Components), Skipped: []taskcommon.SkippedTarget{}}
			}
			continue
		}
		if selected == nil || len(selected.Components) == 0 {
			return nil, flowerrors.GRPCErrorPreconditionFailed(fmt.Sprintf("rack %s has no selected components", id))
		}
		// Resolve the rule before partitioning so expectation-only ingestion
		// uses the same addressability contract as execution.
		var ruleDefExpected bool
		if req.Operation.Type == taskcommon.TaskTypeBringUp && req.Operation.Code == taskcommon.OpCodeIngest {
			rule, err := m.resolveOperationRule(ctx, req.Operation, id)
			if err != nil {
				return nil, err
			}
			if rule == nil {
				return nil, fmt.Errorf("operation rule is nil")
			}
			ruleDefExpected = ruleUsesOnlyExpectedInventory(&rule.RuleDefinition)
		}
		expectationOnly := (req.Operation.Type == taskcommon.TaskTypeInjectExpectation && req.Operation.Code == taskcommon.OpCodeInjectExpectation) || ruleDefExpected
		macSupported := req.Operation.Type == taskcommon.TaskTypePowerControl || req.Operation.Type == taskcommon.TaskTypeFirmwareControl
		resolution := &taskcommon.TargetResolution{SkipUnaddressableTargets: req.SkipUnaddressableTargets, Selected: len(selected.Components), Skipped: []taskcommon.SkippedTarget{}}
		executable := *selected
		executable.Components = nil
		for _, c := range selected.Components {
			if targetAddressable(&c, expectationOnly, macSupported) {
				executable.Components = append(executable.Components, c)
				continue
			}
			resolution.Skipped = append(resolution.Skipped, taskcommon.SkippedTarget{
				ComponentID: c.Info.ID, ComponentType: c.Type,
				SlotID: c.Position.SlotID, TrayIndex: c.Position.TrayIndex, HostID: c.Position.HostID,
				Reason: "unaddressable_target",
			})
		}
		if len(executable.Components) == 0 {
			return nil, flowerrors.GRPCErrorPreconditionFailed(fmt.Sprintf("rack %s has no addressable selected components", id))
		}
		resolution.Submitted = len(executable.Components)
		slices.SortFunc(resolution.Skipped, func(a, b taskcommon.SkippedTarget) int {
			return strings.Compare(a.ComponentID.String(), b.ComponentID.String())
		})
		racks[id] = &executable
		results[id] = resolution
	}
	return results, nil
}

func initializeTargetReport(t *taskdef.Task) error {
	if t.Attributes.TargetResolution == nil {
		return nil
	}
	rep := report.NewInitial(nil, nil)
	rep.TargetResolution = t.Attributes.TargetResolution
	raw, err := rep.MarshalRaw()
	if err != nil {
		return fmt.Errorf("initialize target report: %w", err)
	}
	t.Report = raw
	return nil
}

// submitRackBatch creates every task in one transaction under ordered rack
// locks. After admission all task IDs are returned, including tasks whose
// scheduling fails; their persisted status describes the failure.
func (m *ManagerImpl) submitRackBatch(ctx context.Context, req *operation.Request, racks map[uuid.UUID]*rack.Rack, resolutions map[uuid.UUID]*taskcommon.TargetResolution) ([]uuid.UUID, error) {
	rackIDs := make([]uuid.UUID, 0, len(racks))
	for id := range racks {
		rackIDs = append(rackIDs, id)
	}
	slices.SortFunc(rackIDs, func(a, b uuid.UUID) int { return strings.Compare(a.String(), b.String()) })
	tasks := make([]taskdef.Task, 0, len(rackIDs))
	err := m.taskStore.RunInTransaction(ctx, func(txCtx context.Context) error {
		for _, id := range rackIDs {
			t := newTaskForRack(req, racks[id], resolutions[id])
			t.Attributes.BatchDispatch = taskcommon.BatchDispatchReady
			initializeErr := initializeTargetReport(&t)
			if initializeErr != nil {
				return initializeErr
			}
			conflictErr := m.lockRackAndResolveConflict(txCtx, req, racks[id], &t)
			if conflictErr != nil {
				return conflictErr
			}
			createErr := m.taskStore.CreateTask(txCtx, &t)
			if createErr != nil {
				return createErr
			}
			tasks = append(tasks, t)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	ids := make([]uuid.UUID, len(tasks))
	for i := range tasks {
		ids[i] = tasks[i].ID
	}

	// Earlier rack RPCs can yield while a later task is cancelled. Reload each
	// task immediately before dispatch instead of using its admission-time status.
	// Bookkeeping uses bounded contexts detached from caller cancellation.
	// Dispatch shares the caller's deadline; after admission, bookkeeping errors
	// must not hide the batch's durable IDs or prevent remaining dispatch attempts.
	for _, id := range ids {
		loadCtx, cancelLoad := context.WithTimeout(context.WithoutCancel(ctx), schedulingCleanupTimeout)
		t, loadErr := m.taskStore.GetTask(loadCtx, id)
		cancelLoad()
		if loadErr != nil {
			requeueErr := m.requeuePendingDispatch(ctx, id, m.getReqExpiresAt(req), loadErr)
			log.Error().Err(errors.Join(loadErr, requeueErr)).Str("task_id", id.String()).Msg("could not reload admitted task before dispatch")
			continue
		}
		if t == nil {
			log.Error().Str("task_id", id.String()).Msg("admitted task was not found before dispatch")
			continue
		}
		if t.IsScheduled() || t.Status != taskcommon.TaskStatusPending {
			continue
		}
		executionErr := m.resolveAndExecuteBatchTask(ctx, t, racks[t.RackID])
		if executionErr == nil {
			continue
		}
		if t.Attributes.BatchDispatch == taskcommon.BatchDispatchStarted && !t.IsScheduled() && t.Status != taskcommon.TaskStatusWaiting {
			statusCtx, cancelStatus := context.WithTimeout(context.WithoutCancel(ctx), schedulingCleanupTimeout)
			updateErr := m.taskStore.UpdateTaskStatus(statusCtx, &taskdef.TaskStatusUpdate{ID: t.ID, Status: taskcommon.TaskStatusFailed, Message: message.ForFailure(executionErr)})
			cancelStatus()
			if updateErr != nil {
				log.Error().Err(updateErr).Str("task_id", t.ID.String()).Msg("could not record admitted task scheduling failure")
			}
		}
		log.Error().Err(executionErr).Str("task_id", t.ID.String()).Msg("admitted task could not be scheduled")
	}
	return ids, nil
}

// requeuePendingDispatch leaves accepted work discoverable by the promoter
// after a read failure. The database predicate preserves concurrent cancellation
// and scheduling. A nil deadline retains the original queue expiry on promotion.
func (m *ManagerImpl) requeuePendingDispatch(ctx context.Context, id uuid.UUID, deadline *time.Time, cause error) error {
	statusCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), schedulingCleanupTimeout)
	defer cancel()
	err := m.taskStore.UpdateTaskStatus(statusCtx, &taskdef.TaskStatusUpdate{
		ID: id, Status: taskcommon.TaskStatusWaiting,
		Message:        fmt.Sprintf("Waiting to retry task dispatch: %v", cause),
		QueueExpiresAt: deadline, OnlyIfPendingUnscheduled: true,
	})
	if err != nil {
		return fmt.Errorf("requeue pending dispatch: %w", err)
	}
	return nil
}

// recoverBatchDispatches handles only work still before the durable attempt
// boundary. Claimed work may have been accepted externally and is not replayed.
func (m *ManagerImpl) recoverBatchDispatches(ctx context.Context) {
	tasks, err := m.taskStore.ListPendingBatchDispatches(ctx)
	if err != nil {
		log.Error().Err(err).Msg("could not list undispatched batch tasks")
		return
	}
	for _, t := range tasks {
		if ctx.Err() != nil {
			return
		}
		dispatchCtx, cancel := context.WithTimeout(ctx, batchDispatchRecoveryTimeout)
		err := m.promoteTask(dispatchCtx, t.ID)
		cancel()
		if err != nil {
			log.Error().Err(err).Str("task_id", t.ID.String()).Msg("could not recover batch task dispatch")
		}
	}
}

func (m *ManagerImpl) runBatchDispatchRecovery(ctx context.Context) {
	m.recoverBatchDispatches(ctx)
	ticker := time.NewTicker(batchDispatchRecoveryInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			m.recoverBatchDispatches(ctx)
		}
	}
}

// resolveAndExecuteBatchTask finishes fallible planning before crossing the
// durable attempt boundary shared by foreground and background dispatch.
func (m *ManagerImpl) resolveAndExecuteBatchTask(ctx context.Context, t *taskdef.Task, targetRack *rack.Rack) error {
	rule, err := m.resolveOperationRule(ctx, t.Operation, t.RackID)
	if err == nil && rule == nil {
		err = fmt.Errorf("resolver returned nil rule")
	}
	if err != nil {
		deadline := t.QueueExpiresAt
		if deadline == nil {
			timeout := m.defaultQueueTimeout
			if timeout <= 0 {
				timeout = defaultQueueTimeout
			}
			expires := time.Now().Add(timeout)
			deadline = &expires
		}
		requeueErr := m.requeuePendingDispatch(ctx, t.ID, deadline, err)
		if requeueErr == nil {
			t.Status = taskcommon.TaskStatusWaiting
		}
		return errors.Join(err, requeueErr)
	}
	claimed, err := m.taskStore.ClaimBatchDispatch(ctx, t.ID)
	if err != nil {
		return err
	}
	if !claimed {
		return nil
	}
	t.Attributes.BatchDispatch = taskcommon.BatchDispatchStarted
	err = m.executeResolvedTask(ctx, t, targetRack, rule, false)
	return m.handleSchedulingPersistenceFailure(ctx, err, true)
}
