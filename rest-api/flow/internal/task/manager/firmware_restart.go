// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"fmt"
	"slices"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

func (m *ManagerImpl) newFirmwareTaskForRack(
	ctx context.Context, req *operation.Request, selected *rack.Rack,
) (taskdef.Task, error) {
	task := newTaskForRack(req, selected)
	if req.Operation.Type != taskcommon.TaskTypeFirmwareControl ||
		len(task.Attributes.ComponentsByType[devicetypes.ComponentTypeNVSwitch]) == 0 {
		return task, nil
	}
	var info operations.FirmwareControlTaskInfo
	err := json.Unmarshal(req.Operation.Info, &info)
	if err != nil {
		return task, fmt.Errorf("decode firmware operation: %w", err)
	}
	rule, err := m.resolveOperationRule(ctx, req.Operation, task.RackID)
	if err != nil {
		return task, err
	}
	if rule == nil {
		return task, fmt.Errorf("resolver returned nil rule")
	}
	restartRequired := rule.RuleDefinition.RequiresNVOSComputeRestart(&info)
	task.Attributes.FirmwareRestartRequired = &restartRequired
	if !restartRequired {
		return task, nil
	}

	fullRack, err := m.inventoryStore.GetRackByIdentifier(ctx, identifier.Identifier{ID: task.RackID}, true)
	if err != nil {
		return task, fmt.Errorf("resolve NVOS restart compute: %w", err)
	}
	if fullRack == nil || fullRack.Info.ID != task.RackID {
		return task, fmt.Errorf("NVOS restart rack %s not found", task.RackID)
	}
	// An older manager must still see only the original firmware selection.
	// Keep restart-only membership in an additive field, never in that map.
	reserved := rack.New(fullRack.Info, fullRack.Loc)
	for _, comp := range fullRack.Components {
		if comp.Type == devicetypes.ComponentTypeCompute {
			task.Attributes.FirmwareRestartComputeIDs = append(task.Attributes.FirmwareRestartComputeIDs, comp.Info.ID)
		}
		if comp.Type == devicetypes.ComponentTypeCompute ||
			slices.Contains(task.Attributes.ComponentsByType[comp.Type], comp.Info.ID) {
			reserved.AddComponent(comp)
		}
	}
	err = validateResolvedRackTargets(req.Operation, nil, map[uuid.UUID]*rack.Rack{task.RackID: reserved})
	if err != nil {
		return task, fmt.Errorf("NVOS restart targets: %w", err)
	}
	if len(reserved.Components) != len(task.Attributes.AllComponentUUIDs()) {
		return task, fmt.Errorf("NVOS task inventory changed: a reserved component is missing")
	}
	return task, nil
}
