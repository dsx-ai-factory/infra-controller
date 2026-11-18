// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/mocks"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/workflow"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExecuteWorkflow_FirmwareTimeout(t *testing.T) {
	info := &operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade}
	req := &task.ExecutionRequest{Async: true, Info: task.ExecutionInfo{
		TaskID:     uuid.New(),
		Components: []task.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}},
		RuleDefinition: &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{
			ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, Timeout: time.Minute,
			RetryPolicy: &operationrules.RetryPolicy{MaxAttempts: 1, InitialInterval: time.Second, BackoffCoefficient: 2},
			MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl,
				Parameters: map[string]any{operationrules.ParamPollTimeout: "60m"}},
		}}},
	}}
	desc := workflow.WorkflowDescriptor{WorkflowName: "FirmwareControl", Timeout: time.Hour,
		Unmarshal: func(json.RawMessage) (any, error) { return info, nil },
	}
	run := &mocks.WorkflowRun{}
	run.On("GetID").Return(req.Info.TaskID.String())
	run.On("GetRunID").Return("run-1")
	temporalClient := &mocks.Client{}
	temporalClient.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(opts client.StartWorkflowOptions) bool {
		return opts.WorkflowExecutionTimeout == 123*time.Minute
	}), "FirmwareControl", req.Info, info).Return(run, nil).Once()
	_, err := executeWorkflow(context.Background(), temporalClient, desc, req)
	require.NoError(t, err)
	temporalClient.AssertExpectations(t)
}
