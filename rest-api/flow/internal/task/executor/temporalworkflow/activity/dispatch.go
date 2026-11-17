// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"fmt"
	"strings"

	"github.com/google/uuid"
	temporalactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/dispatchguard"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

type taskReader interface {
	GetTask(context.Context, uuid.UUID) (*taskdef.Task, error)
}

func (a *Activities) dispatchContext(ctx context.Context) context.Context {
	// The production updater embeds the persisted task store. Lightweight test
	// updaters without task lookup keep their existing direct-call behavior.
	reader, ok := a.updater.(taskReader)
	if !ok {
		return ctx
	}
	return dispatchguard.WithCheck(ctx, func(ctx context.Context) error {
		workflowID := temporalactivity.GetInfo(ctx).WorkflowExecution.ID
		taskID, err := dispatchTaskID(workflowID)
		if err != nil {
			return err
		}
		if checker, ok := a.updater.(interface {
			CheckTaskDispatch(context.Context, uuid.UUID) error
		}); ok {
			return checker.CheckTaskDispatch(ctx, taskID)
		}
		task, err := reader.GetTask(ctx, taskID)
		if err != nil {
			return err
		}
		if task == nil {
			return fmt.Errorf("dispatch task %s not found", taskID)
		}
		if task.Status.IsFinished() {
			return temporal.NewNonRetryableApplicationError("task no longer owns hardware dispatch", "TaskPreempted", nil)
		}
		return nil
	})
}

// Flow's root workflow ID is the task UUID; component children use the stable
// component-step-<task UUID>-<component type> name, including predecessor runs.
func dispatchTaskID(workflowID string) (uuid.UUID, error) {
	if strings.HasPrefix(workflowID, "component-step-") {
		suffix := strings.TrimPrefix(workflowID, "component-step-")
		if len(suffix) <= 37 || suffix[36] != '-' {
			return uuid.Nil, fmt.Errorf("invalid component workflow ID")
		}
		workflowID = suffix[:36]
	}
	return uuid.Parse(workflowID)
}
