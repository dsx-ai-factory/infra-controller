// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestSingleLineError(t *testing.T) {
	for _, tc := range []struct{ name, diagnostic, want string }{
		{"single line", "query failed", "query failed"},
		{"multiline", " query failed\r\n\tconnection refused ", "query failed connection refused"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cause := temporal.NewNonRetryableApplicationError(tc.diagnostic, "QueryRejected", nil)
			wrapped := fmt.Errorf("last status query failed: %w", singleLineError{cause})
			require.Contains(t, wrapped.Error(), tc.want)
			require.NotContains(t, wrapped.Error(), "\n")
			require.ErrorIs(t, wrapped, cause)
			var applicationError *temporal.ApplicationError
			require.ErrorAs(t, wrapped, &applicationError)
			require.Same(t, cause, applicationError)
			require.Equal(t, "QueryRejected", applicationError.Type())
			require.True(t, applicationError.NonRetryable())
		})
	}
}

func TestUpdateFinishedTaskStatus(t *testing.T) {
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	store := taskstore.NewPostgres(session)
	for _, tc := range []struct{ name, diagnostic, want string }{
		{"multiline query timeout", "query failed\nconnection refused", "query failed connection refused"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			taskID := uuid.New()
			raw, marshalErr := json.Marshal(&operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn})
			require.NoError(t, marshalErr)
			require.NoError(t, store.CreateTask(t.Context(), &task.Task{
				ID: taskID, RackID: uuid.New(), Status: taskcommon.TaskStatusPending, ExecutorType: taskcommon.ExecutorTypeTemporal,
				Operation: operation.Wrapper{Type: taskcommon.TaskTypePowerControl, Info: raw},
			}))
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			updates := activitypkg.New(store, store, nil, nil)
			env.RegisterActivityWithOptions(updates.UpdateTaskStatus, activity.RegisterOptions{Name: activitypkg.NameUpdateTaskStatus})
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(nil, errors.New(tc.diagnostic))
			env.ExecuteWorkflow(func(ctx workflow.Context) error {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: time.Second, RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
				pollErr := verifyPowerStatus(ctx, common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"compute-1"}}, "on", 2*time.Second, time.Second)
				return updateFinishedTaskStatus(ctx, taskID, pollErr, &report.Report{Version: 1})
			})
			require.Error(t, env.GetWorkflowError())
			// Reload the real terminal write; an in-memory workflow error is insufficient.
			observed, readErr := store.GetTask(context.Background(), taskID)
			require.NoError(t, readErr)
			require.Equal(t, taskcommon.TaskStatusFailed, observed.Status)
			require.Contains(t, observed.Message, tc.want)
			require.Contains(t, observed.Message, "compute-1")
			var rep report.Report
			require.NoError(t, json.Unmarshal(observed.Report, &rep))
			require.Contains(t, rep.Error, tc.want)
			env.AssertExpectations(t)
		})
	}
}
