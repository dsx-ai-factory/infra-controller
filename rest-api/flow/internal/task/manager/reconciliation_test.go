// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type reconciliationExecutor struct {
	executor.Executor
	check func(context.Context, string) (*taskdef.ExecutionStatus, error)
}

func (e *reconciliationExecutor) Start(context.Context) error { return nil }
func (e *reconciliationExecutor) Stop(context.Context) error  { return nil }
func (e *reconciliationExecutor) Type() taskcommon.ExecutorType {
	return taskcommon.ExecutorTypeTemporal
}
func (e *reconciliationExecutor) CheckStatus(ctx context.Context, id string) (*taskdef.ExecutionStatus, error) {
	return e.check(ctx, id)
}

type reconciliationStore struct {
	taskstore.Store
	tasks  []*taskdef.Task
	after  []uuid.UUID
	finish func(*taskdef.Task, *taskdef.ExecutionStatus) (bool, error)
}

func (s *reconciliationStore) ListTasksForReconciliation(_ context.Context, _ taskcommon.ExecutorType, after uuid.UUID, limit int) ([]*taskdef.Task, error) {
	s.after = append(s.after, after)
	start := 0
	if after != uuid.Nil {
		for i, task := range s.tasks {
			if task.ID == after {
				start = i + 1
				break
			}
		}
	}
	end := min(start+limit, len(s.tasks))
	return s.tasks[start:end], nil
}
func (s *reconciliationStore) FinishTaskExecution(_ context.Context, task *taskdef.Task, status *taskdef.ExecutionStatus) (bool, error) {
	return s.finish(task, status)
}

func TestManagerImpl_ReconcileExecutions(t *testing.T) {
	cases := []struct {
		name string
		run  func(*testing.T)
	}{
		{name: "pages past failures with independent RPC deadlines", run: func(t *testing.T) {
			store := &reconciliationStore{}
			for i := 0; i < reconciliationBatchSize+1; i++ {
				store.tasks = append(store.tasks, &taskdef.Task{ID: uuid.New(), ExecutionID: "execution"})
			}
			closed := time.Now().Add(-time.Hour)
			calls, finished := 0, 0
			store.finish = func(_ *taskdef.Task, status *taskdef.ExecutionStatus) (bool, error) {
				require.Equal(t, closed, *status.FinishedAt)
				finished++
				return true, nil
			}
			exec := &reconciliationExecutor{check: func(ctx context.Context, _ string) (*taskdef.ExecutionStatus, error) {
				deadline, ok := ctx.Deadline()
				require.True(t, ok)
				require.Greater(t, time.Until(deadline), reconciliationCallTimeout-time.Second)
				calls++
				if calls == 1 {
					<-ctx.Done()
					return nil, ctx.Err()
				}
				return &taskdef.ExecutionStatus{Status: taskcommon.TaskStatusTerminated, FinishedAt: &closed}, nil
			}}
			m := &ManagerImpl{taskStore: store, executor: exec, promoter: conflict.NewPromoter(nil, nil, conflict.PromoterConfig{})}
			m.reconcileExecutions(t.Context())
			require.Equal(t, reconciliationBatchSize+1, calls)
			require.Equal(t, reconciliationBatchSize, finished)
			require.Equal(t, []uuid.UUID{uuid.Nil, store.tasks[reconciliationBatchSize-1].ID}, store.after)
		}},
		{name: "leaves running and unknown executions untouched", run: func(t *testing.T) {
			for name, status := range map[string]taskcommon.TaskStatus{"running": taskcommon.TaskStatusRunning, "unknown": taskcommon.TaskStatusUnknown} {
				t.Run(name, func(t *testing.T) {
					store := &reconciliationStore{tasks: []*taskdef.Task{{ID: uuid.New(), ExecutionID: "execution"}}, finish: func(*taskdef.Task, *taskdef.ExecutionStatus) (bool, error) {
						t.Fatal("unexpected task closure")
						return false, nil
					}}
					exec := &reconciliationExecutor{check: func(context.Context, string) (*taskdef.ExecutionStatus, error) {
						return &taskdef.ExecutionStatus{Status: status}, nil
					}}
					m := &ManagerImpl{taskStore: store, executor: exec}
					m.reconcileExecutions(t.Context())
				})
			}
		}},
		{name: "restart recovers persisted timeout and promotes queued task", run: func(t *testing.T) {
			if os.Getenv("DB_PORT") == "" {
				t.Skip("PostgreSQL environment is required")
			}
			config, err := cdb.ConfigFromEnv()
			require.NoError(t, err)
			session, err := utils.UnitTestDB(t.Context(), t, config)
			require.NoError(t, err)
			t.Cleanup(session.Close)
			store := taskstore.NewPostgres(session)
			rackID := uuid.New()
			attributes := taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {uuid.New()}}}
			// These records model tasks left by the predecessor, which never queried
			// Temporal after startup. No in-memory ownership from that process remains.
			running := &taskdef.Task{ID: uuid.New(), RackID: rackID, Attributes: attributes, Operation: testPowerControlOperation(t), Status: taskcommon.TaskStatusRunning, ExecutorType: taskcommon.ExecutorTypeTemporal, ExecutionID: "original-run"}
			expires := time.Now().Add(time.Hour)
			waiting := &taskdef.Task{ID: uuid.New(), RackID: rackID, Attributes: attributes, Operation: testPowerControlOperation(t), Status: taskcommon.TaskStatusWaiting, ExecutorType: taskcommon.ExecutorTypeUnknown, QueueExpiresAt: &expires}
			require.NoError(t, store.CreateTask(t.Context(), running))
			require.NoError(t, store.CreateTask(t.Context(), waiting))
			closed := time.Now().Add(-time.Hour).UTC().Truncate(time.Microsecond)
			promoted := make(chan uuid.UUID, 1)
			exec := &reconciliationExecutor{check: func(_ context.Context, id string) (*taskdef.ExecutionStatus, error) {
				if id != "original-run" {
					return nil, errors.New("unexpected execution")
				}
				return &taskdef.ExecutionStatus{Status: taskcommon.TaskStatusTerminated, FinishedAt: &closed, Message: "Temporal workflow timed out; hardware effects are not verified"}, nil
			}}
			promoter := conflict.NewPromoter(store, func(_ context.Context, id uuid.UUID) error { promoted <- id; return nil }, conflict.PromoterConfig{SweepInterval: time.Hour})
			m := &ManagerImpl{taskStore: store, executor: exec, promoter: promoter}
			require.NoError(t, m.Start(t.Context()))
			t.Cleanup(func() { m.Stop(context.Background()) })
			select {
			case id := <-promoted:
				require.Equal(t, waiting.ID, id)
			case <-time.After(5 * time.Second):
				t.Fatal("queued task was not promoted")
			}
			got, err := store.GetTask(t.Context(), running.ID)
			require.NoError(t, err)
			require.Equal(t, taskcommon.TaskStatusTerminated, got.Status)
			require.Equal(t, closed, got.FinishedAt.UTC())
			require.Contains(t, got.Message, "timed out")
			require.Nil(t, got.QueueExpiresAt)
			got, err = store.GetTask(t.Context(), waiting.ID)
			require.NoError(t, err)
			require.Equal(t, taskcommon.TaskStatusPending, got.Status)
			// Late workflow callbacks must not resurrect or replace the timeout.
			for _, status := range []taskcommon.TaskStatus{taskcommon.TaskStatusRunning, taskcommon.TaskStatusCompleted} {
				require.NoError(t, store.UpdateTaskStatus(t.Context(), &taskdef.TaskStatusUpdate{ID: running.ID, Status: status, Message: "late callback"}))
			}
			got, err = store.GetTask(t.Context(), running.ID)
			require.NoError(t, err)
			require.Equal(t, taskcommon.TaskStatusTerminated, got.Status)
			require.Equal(t, closed, got.FinishedAt.UTC())
			// Repeated reconciliation is idempotent and does not restart the old task.
			m.reconcileExecutions(t.Context())
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, tc.run)
	}
}
