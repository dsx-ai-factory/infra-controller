// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"testing"
)

func TestTaskSubmissionStatusError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		code codes.Code
	}{
		{name: "success", code: codes.OK},
		{name: "deadline is preserved", err: context.DeadlineExceeded, code: codes.DeadlineExceeded},
		{name: "cancellation is preserved", err: fmt.Errorf("submit: %w", context.Canceled), code: codes.Canceled},
		{name: "wrapped conflict", err: fmt.Errorf("required rack: %w", taskmanager.ErrRackConflict), code: codes.FailedPrecondition},
		{name: "all queues full", err: errors.Join(taskmanager.ErrQueueFull, fmt.Errorf("rack B: %w", taskmanager.ErrQueueFull)), code: codes.ResourceExhausted},
		{name: "mixed deadline and rejection", err: errors.Join(taskmanager.ErrRackConflict, context.DeadlineExceeded), code: codes.Unknown},
		{name: "mixed rejection causes", err: errors.Join(taskmanager.ErrQueueFull, taskmanager.ErrRackConflict), code: codes.Unknown},
		{name: "operational error takes precedence", err: errors.Join(taskmanager.ErrRackConflict, errors.New("database unavailable")), code: codes.Unknown},
		{name: "existing status", err: fmt.Errorf("validate: %w", status.Error(codes.NotFound, "rule missing")), code: codes.NotFound},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := taskSubmissionStatusError(tt.err)
			require.Equal(t, tt.code, status.Code(err))
			if tt.name == "mixed deadline and rejection" {
				_, ok := status.FromError(err)
				require.True(t, ok, "mixed failure must carry an explicit status")
			}
			if tt.name == "existing status" {
				require.Same(t, tt.err, err)
			}
		})
	}
}
