// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// reconcileTargets copies child task status back into operation-run targets,
// recording any target changes for persistence.
func (d *Dispatcher) reconcileTargets(
	ctx context.Context,
	targets []*operationrun.OperationRunTarget,
	changed map[uuid.UUID]*operationrun.OperationRunTarget,
) error {
	for _, target := range targets {
		if target.TaskID != nil && !target.Status.IsTerminal() {
			task, err := d.deps.TaskStore.GetTask(ctx, *target.TaskID)
			if err != nil {
				return fmt.Errorf(
					"get child task %s: %w",
					*target.TaskID,
					err,
				)
			}

			newStatus := operationrun.OperationRunTargetStatusFromTaskStatus(task.Status)
			if newStatus != target.Status || task.Message != target.Message {
				target.Status = newStatus
				target.SetMessage(task.Message)
				changed[target.ID] = target
			}

		}
	}

	return nil
}
