// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

func TestExecutionRequestValidate(t *testing.T) {
	valid := newValidExecutionRequest(t)
	tests := map[string]struct {
		mutate  func(*ExecutionRequest)
		wantErr string
	}{
		"valid": {},
		"invalid execution id": {
			mutate:  func(request *ExecutionRequest) { request.ExecutionID = uuid.Nil },
			wantErr: "execution id is required",
		},
		"missing plan": {
			mutate:  func(request *ExecutionRequest) { request.Plan = nil },
			wantErr: "execution plan: execution plan is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			request := valid
			request.Plan = eventrule.CloneExecutionPlan(valid.Plan)
			if test.mutate != nil {
				test.mutate(&request)
			}
			err := request.Validate()
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func newValidExecutionRequest(t *testing.T) ExecutionRequest {
	t.Helper()
	execution, err := eventrule.NewExecution(
		uuid.New(),
		"noop",
		&eventrule.NoopPlan{},
		time.Date(2026, 8, 10, 12, 0, 0, 0, time.UTC),
	)
	require.NoError(t, err)
	return ExecutionRequest{
		ExecutionID: execution.ID,
		Plan:        execution.Plan,
	}
}
