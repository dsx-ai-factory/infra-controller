/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

package workflow

import (
	"context"

	"github.com/stretchr/testify/mock"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"

	taskactivity "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

func mockUpdateTaskStatus(_ context.Context, _ *taskdef.TaskStatusUpdate) error {
	return nil
}

func mockUpdateTaskReport(_ context.Context, _ *taskdef.TaskReportUpdate) error {
	return nil
}

func registerTaskUpdateActivities(env *testsuite.TestWorkflowEnvironment) {
	env.RegisterActivityWithOptions(mockUpdateTaskStatus, activity.RegisterOptions{
		Name: taskactivity.NameUpdateTaskStatus,
	})
	env.RegisterActivityWithOptions(mockUpdateTaskReport, activity.RegisterOptions{
		Name: taskactivity.NameUpdateTaskReport,
	})
}

func expectTaskUpdateActivities(env *testsuite.TestWorkflowEnvironment) {
	env.OnActivity(taskactivity.NameUpdateTaskStatus, mock.Anything, mock.Anything).Return(nil)
	env.OnActivity(taskactivity.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(nil)
}
