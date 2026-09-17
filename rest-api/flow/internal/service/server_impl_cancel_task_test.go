// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

type cancelTaskManager struct {
	taskmanager.Manager
	cancelErr error
}

func (m *cancelTaskManager) CancelTask(context.Context, uuid.UUID) error {
	return m.cancelErr
}

func TestFlowServerImpl_CancelTask(t *testing.T) {
	taskID := uuid.New()
	server := &FlowServerImpl{taskManager: &cancelTaskManager{
		cancelErr: fmt.Errorf("%w: task %s has status failed", taskmanager.ErrTaskNotCancellable, taskID),
	}}

	response, err := server.CancelTask(context.Background(), &pb.CancelTaskRequest{
		TaskId: &pb.UUID{Id: taskID.String()},
	})

	require.Nil(t, response)
	require.Equal(t, codes.FailedPrecondition, status.Code(err))
	require.ErrorContains(t, err, "status failed")
}
