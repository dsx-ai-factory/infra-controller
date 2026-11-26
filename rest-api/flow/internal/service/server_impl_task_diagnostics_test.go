// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	sdkworkflow "go.temporal.io/sdk/workflow"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	switchmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/nvswitch/nico"
	shelfmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/powershelf/nico"
	taskactivity "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	targetcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	taskworkflow "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/workflow"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// diagnosticCoreClient supplies the external failure; the adapters, workflow,
// status activities, PostgreSQL store and public task RPC run their real code.
type diagnosticCoreClient struct {
	nicoapi.Client
	componentID       string
	reason            string
	resetCalls        int
	secondComponentID string
}

func (c *diagnosticCoreClient) ComponentReset(context.Context, *corev1.ComponentResetRequest) (*corev1.ComponentResetResponse, error) {
	c.resetCalls++
	return &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String(c.componentID)}}}, nil
}

func (c *diagnosticCoreClient) GetComponentResetStatus(context.Context, *corev1.GetComponentResetStatusRequest) (*corev1.GetComponentResetStatusResponse, error) {
	return &corev1.GetComponentResetStatusResponse{Statuses: []*corev1.ComponentResetStatus{{
		Result: &corev1.ComponentResult{ComponentId: proto.String(c.componentID), Error: c.reason},
		Action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET,
		State:  corev1.ComponentResetState_COMPONENT_RESET_STATE_FAILED,
	}}}, nil
}

func (c *diagnosticCoreClient) GetComponentFirmwareStatus(context.Context, *corev1.GetComponentFirmwareStatusRequest) (*corev1.GetComponentFirmwareStatusResponse, error) {
	response := &corev1.GetComponentFirmwareStatusResponse{Statuses: []*corev1.FirmwareUpdateStatus{{
		Result: &corev1.ComponentResult{ComponentId: proto.String(c.componentID), Error: c.reason},
		State:  corev1.FirmwareUpdateState_FW_STATE_FAILED,
	}}}
	if c.secondComponentID != "" {
		response.Statuses = append(response.Statuses, &corev1.FirmwareUpdateStatus{Result: &corev1.ComponentResult{ComponentId: proto.String(c.secondComponentID), Error: "image incompatible"}, State: corev1.FirmwareUpdateState_FW_STATE_FAILED})
	}
	return response, nil
}

func TestFlowServerImpl_GetTasksByIDs(t *testing.T) {
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	store := taskstore.NewPostgres(session)
	server := &FlowServerImpl{taskStore: store}

	for _, tc := range []struct {
		name          string
		taskType      taskcommon.TaskType
		componentType devicetypes.ComponentType
		reason        string
		secondFailure bool
	}{
		{name: "reset failure reaches persisted task and RPC", taskType: taskcommon.TaskTypeComponentReset, componentType: devicetypes.ComponentTypeNVSwitch, reason: "cluster is not enabled"},
		{name: "shelf firmware failure reaches persisted task and RPC", taskType: taskcommon.TaskTypeFirmwareControl, componentType: devicetypes.ComponentTypePowerShelf, reason: "firmware signature rejected"},
		{name: "long firmware reason preserves both failed identities", taskType: taskcommon.TaskTypeFirmwareControl, componentType: devicetypes.ComponentTypePowerShelf, reason: strings.Repeat("x", 600), secondFailure: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &diagnosticCoreClient{componentID: uuid.NewString(), reason: tc.reason}
			if tc.secondFailure {
				client.componentID = "00000000-0000-4000-8000-000000000001"
				client.secondComponentID = "00000000-0000-4000-8000-000000000002"
			}
			var payload any = &operations.ComponentResetTaskInfo{Target: "sdn"}
			if tc.taskType == taskcommon.TaskTypeFirmwareControl {
				payload = &operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade}
			}
			raw, marshalErr := json.Marshal(payload)
			require.NoError(t, marshalErr)
			info := task.ExecutionInfo{TaskID: uuid.New(), Components: []task.WorkflowComponent{{Type: tc.componentType, ComponentID: client.componentID}}}
			if tc.secondFailure {
				info.Components = append(info.Components, task.WorkflowComponent{Type: tc.componentType, ComponentID: client.secondComponentID})
			}
			info.RuleDefinition = &operationrules.RuleDefinition{Version: "v1", Steps: []operationrules.SequenceStep{{
				ComponentType: tc.componentType, Stage: 1, Timeout: time.Minute,
				MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl},
			}}}
			require.NoError(t, store.CreateTask(t.Context(), &task.Task{
				ID: info.TaskID, RackID: uuid.New(), Status: taskcommon.TaskStatusPending, ExecutorType: taskcommon.ExecutorTypeTemporal,
				Operation: operation.Wrapper{Type: tc.taskType, Info: raw},
			}))

			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			for _, descriptor := range taskworkflow.GetAllWorkflows() {
				env.RegisterWorkflowWithOptions(descriptor.WorkflowFunc, sdkworkflow.RegisterOptions{Name: descriptor.WorkflowName})
			}
			statusActivities := taskactivity.New(store, store, nil, nil)
			env.RegisterActivityWithOptions(statusActivities.UpdateTaskStatus, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskStatus})
			env.RegisterActivityWithOptions(statusActivities.UpdateTaskReport, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskReport})
			if tc.taskType == taskcommon.TaskTypeComponentReset {
				manager := switchmanager.New(client, nil)
				env.RegisterActivityWithOptions(manager.PrepareFactoryReset, sdkactivity.RegisterOptions{Name: taskactivity.NamePrepareFactoryReset})
				env.RegisterActivityWithOptions(manager.SubmitFactoryReset, sdkactivity.RegisterOptions{Name: taskactivity.NameSubmitFactoryReset})
				env.RegisterActivityWithOptions(manager.SDNResetCompleted, sdkactivity.RegisterOptions{Name: taskactivity.NameSDNResetCompleted})
			} else {
				manager := shelfmanager.New(client, nil)
				env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target, operations.FirmwareControlTaskInfo) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameFirmwareControl})
				env.RegisterActivityWithOptions(func(ctx context.Context, target targetcommon.Target) (*taskactivity.GetFirmwareStatusResult, error) {
					statuses, queryErr := manager.GetFirmwareStatus(ctx, target)
					return &taskactivity.GetFirmwareStatusResult{Statuses: statuses}, queryErr
				}, sdkactivity.RegisterOptions{Name: taskactivity.NameGetFirmwareStatus})
			}
			descriptor, ok := taskworkflow.Get(tc.taskType)
			require.True(t, ok)
			env.ExecuteWorkflow(descriptor.WorkflowFunc, info, payload)
			require.ErrorContains(t, env.GetWorkflowError(), tc.reason)
			if tc.taskType == taskcommon.TaskTypeComponentReset {
				require.Equal(t, 1, client.resetCalls)
			}

			// Reload through the RPC, so an in-memory error alone cannot satisfy this proof.
			response, queryErr := server.GetTasksByIDs(t.Context(), &pb.GetTasksByIDsRequest{TaskIds: []*pb.UUID{{Id: info.TaskID.String()}}})
			require.NoError(t, queryErr)
			require.Len(t, response.Tasks, 1)
			observed := response.Tasks[0]
			require.Equal(t, pb.TaskStatus_TASK_STATUS_FAILED, observed.Status)
			require.NotNil(t, observed.FinishedAt)
			if !tc.secondFailure {
				require.Contains(t, observed.Message, tc.reason)
			}
			require.Contains(t, observed.Message, client.componentID)
			var rep report.Report
			require.NoError(t, json.Unmarshal([]byte(observed.Report), &rep))
			if !tc.secondFailure {
				require.Contains(t, rep.Error, tc.reason)
			}
			require.Contains(t, rep.Error, client.componentID)
			wire, wireErr := protojson.Marshal(response)
			require.NoError(t, wireErr)
			if tc.secondFailure {
				require.Contains(t, observed.Message, client.secondComponentID)
				require.Contains(t, rep.Error, client.secondComponentID)
				require.LessOrEqual(t, len(observed.Message), 512)
			} else {
				require.Contains(t, string(wire), tc.reason)
			}
		})
	}
}
