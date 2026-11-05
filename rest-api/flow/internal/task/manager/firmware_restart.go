// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"fmt"
	"maps"
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
	if len(info.SubTargets) != 0 && !slices.Contains(info.SubTargets, "nvos") {
		return task, nil
	}

	fullRack, err := m.inventoryStore.GetRackByIdentifier(ctx, identifier.Identifier{ID: task.RackID}, true)
	if err != nil {
		return task, fmt.Errorf("resolve NVOS restart compute: %w", err)
	}
	if fullRack == nil || fullRack.Info.ID != task.RackID {
		return task, fmt.Errorf("NVOS restart rack %s not found", task.RackID)
	}
	// Both selections live in the same task row. The union participates in
	// normal component conflict checks; the original selection alone flashes.
	task.Attributes.FirmwareTargetsByType = task.Attributes.ComponentsByType
	task.Attributes.ComponentsByType = maps.Clone(task.Attributes.FirmwareTargetsByType)
	computeType := devicetypes.ComponentTypeCompute
	computeIDs := slices.Clone(task.Attributes.ComponentsByType[computeType])
	for _, comp := range fullRack.Components {
		if comp.Type == computeType && !slices.Contains(computeIDs, comp.Info.ID) {
			computeIDs = append(computeIDs, comp.Info.ID)
		}
	}
	if len(computeIDs) > 0 {
		task.Attributes.ComponentsByType[computeType] = computeIDs
	}
	reserved := rack.New(fullRack.Info, fullRack.Loc)
	for _, comp := range fullRack.Components {
		if slices.Contains(task.Attributes.ComponentsByType[comp.Type], comp.Info.ID) {
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
