// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"
	"slices"
	"strings"

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

// resolveSubmissionTargets runs after selector/type filtering. Only linkage
// failures are skippable; missing selectors, invalid rules and readiness remain errors.
func (m *ManagerImpl) resolveSubmissionTargets(ctx context.Context, req *operation.Request, racks map[uuid.UUID]*rack.Rack) (map[uuid.UUID]*taskcommon.TargetResolution, error) {
	results := make(map[uuid.UUID]*taskcommon.TargetResolution, len(racks))
	mode := req.TargetResolutionMode
	if mode == "" {
		mode = taskcommon.TargetResolutionStrict
	}
	for id, selected := range racks {
		if mode == taskcommon.TargetResolutionStrict {
			if selected != nil {
				results[id] = &taskcommon.TargetResolution{Mode: mode, Selected: len(selected.Components), Submitted: len(selected.Components), Skipped: []taskcommon.SkippedTarget{}}
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
		resolution := &taskcommon.TargetResolution{Mode: mode, Selected: len(selected.Components), Skipped: []taskcommon.SkippedTarget{}}
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
			if err := initializeTargetReport(&t); err != nil {
				return err
			}
			if err := m.lockRackAndResolveConflict(txCtx, req, racks[id], &t); err != nil {
				return err
			}
			if err := m.taskStore.CreateTask(txCtx, &t); err != nil {
				return err
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
	for i := range tasks {
		t := &tasks[i]
		if t.Status == taskcommon.TaskStatusWaiting {
			continue
		}
		if err := m.resolveAndExecuteTask(ctx, t, racks[t.RackID]); err != nil {
			if !t.IsScheduled() && t.Status != taskcommon.TaskStatusWaiting {
				if updateErr := m.taskStore.UpdateTaskStatus(ctx, &taskdef.TaskStatusUpdate{ID: t.ID, Status: taskcommon.TaskStatusFailed, Message: message.ForFailure(err)}); updateErr != nil {
					return ids, fmt.Errorf("record admitted task failure: %w", updateErr)
				}
			}
			log.Error().Err(err).Str("task_id", t.ID.String()).Msg("admitted task could not be scheduled")
		}
	}
	return ids, nil
}
