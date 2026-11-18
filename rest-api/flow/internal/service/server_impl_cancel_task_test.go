// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// Embedded interfaces make unexpected calls fail instead of silently succeeding.
type cancelTaskManager struct {
	taskmanager.Manager
	cancelErr   error
	cancelledID uuid.UUID
	calls       int
}

func (m *cancelTaskManager) CancelTask(_ context.Context, id uuid.UUID) error {
	m.calls++
	m.cancelledID = id
	return m.cancelErr
}

type cancelTaskStore struct {
	taskstore.Store
	task   *taskdef.Task
	readID uuid.UUID
}

func (s *cancelTaskStore) GetTask(_ context.Context, id uuid.UUID) (*taskdef.Task, error) {
	s.readID = id
	return s.task, nil
}

func TestFlowServerImpl_CancelTask(t *testing.T) {
	taskID := uuid.New()
	tests := []struct {
		name        string
		id          *pb.UUID
		cancelErr   error
		wantCode    codes.Code
		wantCalls   int
		wantMessage string
	}{
		{name: "missing task ID", wantCode: codes.InvalidArgument, wantMessage: "task_id"},
		{name: "empty task ID", id: &pb.UUID{}, wantCode: codes.InvalidArgument, wantMessage: "task_id"},
		{name: "malformed task ID", id: &pb.UUID{Id: "not-a-uuid"}, wantCode: codes.InvalidArgument, wantMessage: "task_id"},
		{name: "zero task ID", id: &pb.UUID{Id: uuid.Nil.String()}, wantCode: codes.InvalidArgument, wantMessage: "task_id"},
		{name: "accepted cancellation", id: &pb.UUID{Id: taskID.String()}, wantCode: codes.OK, wantCalls: 1},
		{
			name:        "terminal task cannot be cancelled",
			id:          &pb.UUID{Id: taskID.String()},
			cancelErr:   fmt.Errorf("%w: task %s has status failed", taskmanager.ErrTaskNotCancellable, taskID),
			wantCode:    codes.FailedPrecondition,
			wantCalls:   1,
			wantMessage: "status failed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			manager := &cancelTaskManager{cancelErr: tt.cancelErr}
			store := &cancelTaskStore{task: &taskdef.Task{ID: taskID, Status: taskcommon.TaskStatusTerminated}}
			server := &FlowServerImpl{taskManager: manager, taskStore: store}
			client := flowRPCClient(t, server)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()

			response, err := client.CancelTask(ctx, &pb.CancelTaskRequest{TaskId: tt.id})

			require.Equal(t, tt.wantCode, status.Code(err))
			require.Equal(t, tt.wantCalls, manager.calls)
			if tt.wantCalls > 0 {
				require.Equal(t, taskID, manager.cancelledID)
			}
			if tt.wantCode != codes.OK {
				require.Nil(t, response)
				require.ErrorContains(t, err, tt.wantMessage)
				require.Equal(t, uuid.Nil, store.readID)
				return
			}
			require.NoError(t, err)
			require.Equal(t, taskID, store.readID)
			require.Equal(t, taskID.String(), response.GetTask().GetId().GetId())
			require.Equal(t, pb.TaskStatus_TASK_STATUS_TERMINATED, response.GetTask().GetStatus())
		})
	}
}
