// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

func TestPostgresStore_FinishTaskExecution(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("PostgreSQL environment is required")
	}
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	store := NewPostgres(session)
	closed := time.Now().Add(-time.Hour).UTC().Truncate(time.Microsecond)
	snapshot := &taskdef.ExecutionStatus{Status: taskcommon.TaskStatusTerminated, FinishedAt: &closed, Message: "Temporal workflow timed out"}
	for _, tc := range []struct {
		name            string
		status          taskcommon.TaskStatus
		storedExecution string
		changed         bool
	}{
		{"running timeout", taskcommon.TaskStatusRunning, "run-1", true},
		{"pending timeout", taskcommon.TaskStatusPending, "run-1", true},
		{"concurrent completion", taskcommon.TaskStatusCompleted, "run-1", false},
		{"different execution", taskcommon.TaskStatusRunning, "run-2", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			task := &taskdef.Task{ID: uuid.New(), RackID: uuid.New(), Status: tc.status, ExecutionID: tc.storedExecution, ExecutorType: taskcommon.ExecutorTypeTemporal, Message: "original", Operation: operation.Wrapper{Type: taskcommon.TaskTypePowerControl, Code: "ForcePowerOff", Info: []byte(`{}`)}}
			require.NoError(t, store.CreateTask(t.Context(), task))
			expected := *task
			expected.ExecutionID = "run-1"
			changed, err := store.FinishTaskExecution(t.Context(), &expected, snapshot)
			require.NoError(t, err)
			require.Equal(t, tc.changed, changed)
			got, err := store.GetTask(t.Context(), task.ID)
			require.NoError(t, err)
			if tc.changed {
				require.Equal(t, taskcommon.TaskStatusTerminated, got.Status)
				require.Equal(t, closed, got.FinishedAt.UTC())
			} else {
				require.Equal(t, tc.status, got.Status)
				require.Equal(t, "original", got.Message)
			}
		})
	}
	// Invalid snapshots cannot close a task or invent a finish time.
	_, err = store.FinishTaskExecution(context.Background(), &taskdef.Task{ExecutionID: "run"}, &taskdef.ExecutionStatus{Status: taskcommon.TaskStatusTerminated})
	require.Error(t, err)
}

func TestPostgresStore_ListTasksForReconciliation(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("PostgreSQL environment is required")
	}
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	store := NewPostgres(session)
	ids := []uuid.UUID{
		uuid.MustParse("00000000-0000-0000-0000-000000000001"),
		uuid.MustParse("00000000-0000-0000-0000-000000000002"),
		uuid.MustParse("00000000-0000-0000-0000-000000000003"),
	}
	for i, status := range []taskcommon.TaskStatus{taskcommon.TaskStatusRunning, taskcommon.TaskStatusCompleted, taskcommon.TaskStatusPending} {
		require.NoError(t, store.CreateTask(t.Context(), &taskdef.Task{ID: ids[i], RackID: uuid.New(), Status: status, ExecutionID: "run", ExecutorType: taskcommon.ExecutorTypeTemporal, Operation: operation.Wrapper{Type: taskcommon.TaskTypePowerControl, Code: "ForcePowerOff", Info: []byte(`{}`)}}))
	}
	first, err := store.ListTasksForReconciliation(t.Context(), taskcommon.ExecutorTypeTemporal, uuid.Nil, 1)
	require.NoError(t, err)
	require.Len(t, first, 1)
	require.Equal(t, ids[0], first[0].ID)
	// Removing the first page must not shift the cursor past a surviving row.
	closed := time.Now()
	_, err = store.FinishTaskExecution(t.Context(), first[0], &taskdef.ExecutionStatus{Status: taskcommon.TaskStatusTerminated, FinishedAt: &closed})
	require.NoError(t, err)
	next, err := store.ListTasksForReconciliation(t.Context(), taskcommon.ExecutorTypeTemporal, first[0].ID, 1)
	require.NoError(t, err)
	require.Len(t, next, 1)
	require.Equal(t, ids[2], next[0].ID)
}
