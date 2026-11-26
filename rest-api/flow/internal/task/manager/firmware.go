// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// preparedFirmwareTask keeps preflight and persistence on the same admission
// snapshot while keeping restart-only devices out of firmware recipients.
type preparedFirmwareTask struct {
	task            taskdef.Task
	additionalSteps []taskdef.ExecutionStep
}

func (m *ManagerImpl) newFirmwareTaskForRack(
	ctx context.Context, req *operation.Request, selected *rack.Rack, rule *operationrules.OperationRule,
) (*preparedFirmwareTask, error) {
	prepared := &preparedFirmwareTask{task: newTaskForRack(ctx, req, selected)}
	task := &prepared.task
	if req.Operation.Type != taskcommon.TaskTypeFirmwareControl ||
		len(task.Attributes.ComponentsByType[devicetypes.ComponentTypeNVSwitch]) == 0 {
		return prepared, nil
	}
	var info operations.FirmwareControlTaskInfo
	err := json.Unmarshal(req.Operation.Info, &info)
	if err != nil {
		return nil, fmt.Errorf("decode firmware operation: %w", err)
	}
	if rule == nil {
		return nil, fmt.Errorf("resolver returned nil rule")
	}
	restartRequired := rule.RuleDefinition.RequiresNVOSComputeRestart(&info)
	task.Attributes.FirmwareRestartRequired = &restartRequired
	if !restartRequired {
		return prepared, nil
	}

	ids, components, err := m.prepareRackComputeRestart(ctx, task)
	if err != nil {
		return nil, fmt.Errorf("NVOS restart targets: %w", err)
	}
	task.Attributes.FirmwareRestartComputeIDs = ids
	prepared.additionalSteps, err = buildRestartSteps(&rule.RuleDefinition, components)
	if err != nil {
		return nil, err
	}
	return prepared, nil
}
