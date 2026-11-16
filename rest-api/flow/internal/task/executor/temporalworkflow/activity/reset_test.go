// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	nvswitchnico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/nvswitch/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	workflowcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type resetOwnershipStore struct {
	task.TaskStatusUpdater
	current task.Task
	readID  uuid.UUID
	reads   int
}

func (s *resetOwnershipStore) GetTask(_ context.Context, id uuid.UUID) (*task.Task, error) {
	s.readID = id
	s.reads++
	return &s.current, nil
}

type resetOwnershipClient struct {
	nicoapi.Client
	calls int
}

func (c *resetOwnershipClient) FindSwitchRackIDs(context.Context, []string) (map[string]string, error) {
	return map[string]string{"switch-1": "rack-1"}, nil
}

func (c *resetOwnershipClient) ComponentReset(context.Context, *corev1.ComponentResetRequest) (*corev1.ComponentResetResponse, error) {
	c.calls++
	return &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1")}}}, nil
}

type resetOwnershipGate struct {
	readiness.Gate
	store   *resetOwnershipStore
	preempt bool
	calls   int
}

func (g *resetOwnershipGate) WaitForRackHostsReady(context.Context, []string, types.OperationType) error {
	g.calls++
	// Simulate the durable preemption transition while readiness is waiting,
	// immediately before it reports that the rack is ready again.
	if g.preempt {
		g.store.current.Status = common.TaskStatusTerminated
	}
	return nil
}

func TestActivities_PrepareFactoryReset(t *testing.T) {
	checkResetActivityOwnership(t, NamePrepareFactoryReset)
}

func TestActivities_SubmitFactoryReset(t *testing.T) {
	checkResetActivityOwnership(t, NameSubmitFactoryReset)
}

func checkResetActivityOwnership(t *testing.T, activityName string) {
	t.Helper()
	for _, tc := range []struct {
		name    string
		preempt bool
	}{
		{name: "running task retains dispatch"},
		{name: "task preempted while readiness waits", preempt: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			taskID := uuid.New()
			store := &resetOwnershipStore{current: task.Task{ID: taskID, Status: common.TaskStatusRunning}}
			core := &resetOwnershipClient{}
			gate := &resetOwnershipGate{store: store, preempt: tc.preempt}
			manager := nvswitchnico.New(core, gate)
			registry, err := componentmanager.NewRegistry(
				[]componentmanager.FactorySpec{{Descriptor: manager.Descriptor(), Factory: func(*providerapi.ProviderRegistry) (componentmanager.ComponentManager, error) { return manager, nil }}},
				cmconfig.Config{ComponentManagers: map[devicetypes.ComponentType]string{devicetypes.ComponentTypeNVSwitch: nvswitchnico.ImplementationName}},
				providerapi.NewProviderRegistry(),
			)
			require.NoError(t, err)
			activities := New(store, nil, registry, nil)
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.SetStartWorkflowOptions(client.StartWorkflowOptions{ID: taskID.String()})
			env.RegisterActivity(activities.PrepareFactoryReset)
			env.RegisterActivity(activities.SubmitFactoryReset)
			env.ExecuteWorkflow(func(ctx workflow.Context) error {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: time.Minute, RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
				target := workflowcommon.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{"switch-1"}}
				if activityName == NamePrepareFactoryReset {
					return workflow.ExecuteActivity(ctx, activityName, target).Get(ctx, nil)
				}
				return workflow.ExecuteActivity(ctx, activityName, target, "nvos").Get(ctx, nil)
			})
			if tc.preempt {
				require.Error(t, env.GetWorkflowError())
				require.Equal(t, common.TaskStatusTerminated, store.current.Status)
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
			require.Equal(t, taskID, store.readID)
			require.GreaterOrEqual(t, store.reads, 2, "ownership must be read both before and after readiness")
			require.Equal(t, 1, gate.calls)
			wantCalls := 0
			if activityName == NameSubmitFactoryReset && !tc.preempt {
				wantCalls = 1
			}
			require.Equal(t, wantCalls, core.calls)
		})
	}
}
