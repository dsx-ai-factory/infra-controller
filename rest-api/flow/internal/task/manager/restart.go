// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"
	"slices"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// prepareRackComputeRestart snapshots rack compute and validates the union with
// the task's existing targets without changing its primary operation selection.
func (m *ManagerImpl) prepareRackComputeRestart(
	ctx context.Context, task *taskdef.Task,
) ([]uuid.UUID, []taskdef.WorkflowComponent, error) {
	fullRack, err := m.inventoryStore.GetRackByIdentifier(ctx, identifier.Identifier{ID: task.RackID}, true)
	if err != nil {
		return nil, nil, fmt.Errorf("resolve restart compute: %w", err)
	}
	if fullRack == nil || fullRack.Info.ID != task.RackID {
		return nil, nil, fmt.Errorf("restart rack %s not found", task.RackID)
	}

	var ids []uuid.UUID
	var components []taskdef.WorkflowComponent
	selectedIDs := task.Attributes.AllComponentUUIDs()
	reserved := rack.New(fullRack.Info, fullRack.Loc)
	for _, comp := range fullRack.Components {
		if comp.Type == devicetypes.ComponentTypeCompute {
			ids = append(ids, comp.Info.ID)
			components = append(components, taskdef.WorkflowComponent{
				Type: comp.Type, ComponentID: comp.ComponentID, MACAddress: comp.ManagementMAC(),
			})
			if !slices.Contains(selectedIDs, comp.Info.ID) {
				selectedIDs = append(selectedIDs, comp.Info.ID)
			}
		}
		if comp.Type == devicetypes.ComponentTypeCompute ||
			slices.Contains(task.Attributes.ComponentsByType[comp.Type], comp.Info.ID) {
			reserved.AddComponent(comp)
		}
	}

	err = validateResolvedRackTargets(task.Operation, nil, map[uuid.UUID]*rack.Rack{task.RackID: reserved})
	if err != nil {
		return nil, nil, err
	}
	if len(reserved.Components) != len(selectedIDs) {
		return nil, nil, fmt.Errorf("restart inventory changed: a reserved component is missing")
	}
	return ids, components, nil
}

// buildRestartSteps binds the restart sequence after every stage in the main rule.
func buildRestartSteps(
	rule *operationrules.RuleDefinition, components []taskdef.WorkflowComponent,
) ([]taskdef.ExecutionStep, error) {
	if len(components) == 0 {
		return nil, nil
	}
	if rule == nil {
		return nil, fmt.Errorf("restart steps require a main rule")
	}
	lastStage := rule.GetMaxStage()
	var steps []taskdef.ExecutionStep
	for _, step := range operationrules.ComputeRestartSteps() {
		if lastStage == int(^uint(0)>>1) {
			return nil, fmt.Errorf("no stage number available for additional operation steps")
		}
		lastStage++
		step.Stage = lastStage
		steps = append(steps, taskdef.ExecutionStep{Definition: step, Components: slices.Clone(components)})
	}
	return steps, nil
}
