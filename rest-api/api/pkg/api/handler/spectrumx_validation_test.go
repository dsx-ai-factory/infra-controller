// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Check the actual handler-dispatched contexts, not just timeout constants: a
// workflow parented by discovery would inherit its earlier deadline.
func testSpectrumXWorkflowBudget(t *testing.T, calls []mock.Call) {
	t.Helper()
	var discoveryDeadline, workflowDeadline time.Time
	for _, call := range calls {
		if call.Method != "ExecuteWorkflow" {
			continue
		}
		ctx := call.Arguments.Get(0).(context.Context)
		deadline, ok := ctx.Deadline()
		require.True(t, ok)
		if call.Arguments.Get(2) == grpcproxy.Core.WorkflowName {
			discoveryDeadline = deadline
		} else {
			workflowDeadline = deadline
		}
	}
	require.False(t, discoveryDeadline.IsZero())
	require.False(t, workflowDeadline.IsZero())
	assert.Greater(t, workflowDeadline.Sub(discoveryDeadline), cutil.WorkflowExecutionTimeout)
}

// Delay one transaction query until preparation expires. The database connection
// remains real, so the handler must roll back its writes without dispatching Core.
type testSpectrumXQueryDelay struct {
	deadline time.Time
}

func (h *testSpectrumXQueryDelay) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	if !h.deadline.IsZero() {
		time.Sleep(time.Until(h.deadline) + time.Millisecond)
		h.deadline = time.Time{}
	}
	return ctx
}

func (*testSpectrumXQueryDelay) AfterQuery(context.Context, *bun.QueryEvent) {}

func testSpectrumXMachine(id, device string, count uint32) *corev1.Machine {
	kind := corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPECTRUM_X
	return &corev1.Machine{
		Id: &corev1.MachineId{Id: id},
		Status: &corev1.MachineStatus{Capabilities: &corev1.MachineCapabilitiesSet{
			Network: []*corev1.MachineCapabilityAttributesNetwork{{Name: device, Count: count, DeviceType: &kind}},
		}},
	}
}

// The fixture only returns explicitly declared inventory, so missing IDs and
// stale REST capabilities cannot accidentally become compatible in tests.
func testSpectrumXDiscovery(t *testing.T, client *tmocks.Client, inventory map[string]*corev1.Machine, lookupErr error) *mock.Call {
	t.Helper()
	return client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Return(func(_ context.Context, _ tclient.StartWorkflowOptions, _ interface{}, args ...interface{}) tclient.WorkflowRun {
			proxyReq := args[0].(grpcproxy.Request)
			assert.Equal(t, corev1.Forge_FindMachinesByIds_FullMethodName, proxyReq.FullMethod)
			var req corev1.MachinesByIdsRequest
			require.NoError(t, protojson.Unmarshal(proxyReq.RequestJSON, &req))
			assert.False(t, req.IncludeHistory)
			response := &corev1.MachineList{}
			for _, id := range req.MachineIds {
				machine := inventory[id.Id]
				if machine != nil {
					response.Machines = append(response.Machines, machine)
				}
			}
			run := &tmocks.WorkflowRun{}
			if lookupErr != nil {
				run.On("Get", mock.Anything, mock.Anything).Return(lookupErr)
			} else {
				data, err := protojson.Marshal(response)
				require.NoError(t, err)
				run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
					args.Get(1).(*grpcproxy.Response).ResponseJSON = data
				}).Return(nil)
			}
			return run
		}, nil)
}
