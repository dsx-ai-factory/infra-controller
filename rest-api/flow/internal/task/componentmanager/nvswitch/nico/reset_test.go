// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
)

type resetClient struct {
	nicoapi.Client
	response      *corev1.ComponentResetResponse
	observation   *corev1.GetComponentResetStatusResponse
	err           error
	request       *corev1.ComponentResetRequest
	calls         int
	statusRequest *corev1.GetComponentResetStatusRequest
}

func (c *resetClient) ComponentReset(_ context.Context, req *corev1.ComponentResetRequest) (*corev1.ComponentResetResponse, error) {
	c.calls++
	c.request = req
	return c.response, c.err
}
func (c *resetClient) GetComponentResetStatus(_ context.Context, req *corev1.GetComponentResetStatusRequest) (*corev1.GetComponentResetStatusResponse, error) {
	c.statusRequest = req
	return c.observation, c.err
}

func checkFactoryResetSubmissionResults(t *testing.T) {
	for _, tc := range []struct {
		name      string
		response  *corev1.ComponentResetResponse
		err       error
		wantError bool
		wantCause string
		wantType  string
	}{
		{name: "accepted", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1")}}}},
		{name: "lost response", err: status.Error(codes.Unavailable, "response lost"), wantError: true},
		{name: "unsupported Core", err: status.Error(codes.Unimplemented, "unsupported"), wantError: true},
		{name: "missing result", response: &corev1.ComponentResetResponse{}, wantError: true},
		{name: "wrong identity", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-2")}}}, wantError: true},
		{name: "rejected with cause", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1"), Status: corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_INVALID_ARGUMENT, Error: "reset disabled by policy"}}}, wantError: true, wantCause: "reset disabled by policy", wantType: "NVOSResetRejected"},
		{name: "unknown outcome", wantCause: "job reference lost", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1"), Status: corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_OUTCOME_UNKNOWN, Error: "job reference lost"}}}, wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &resetClient{response: tc.response, err: tc.err}
			err := New(client, nil).SubmitFactoryReset(context.Background(), common.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{"switch-1"}}, "nvos")
			require.Equal(t, tc.wantError, err != nil)
			if tc.wantCause != "" {
				require.ErrorContains(t, err, tc.wantCause)
			}
			if tc.wantType != "" {
				var applicationError *temporal.ApplicationError
				require.ErrorAs(t, err, &applicationError)
				require.Equal(t, tc.wantType, applicationError.Type())
				require.True(t, applicationError.NonRetryable())
			}
			require.Equal(t, 1, client.calls)
			require.Equal(t, corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET, client.request.Action)
			require.Equal(t, "switch-1", client.request.GetSwitchIds().Ids[0].Id)
		})
	}
}

func TestManager_NVOSResetCompleted(t *testing.T) {
	for _, tc := range []struct {
		name         string
		state        corev1.ComponentResetState
		resultStatus corev1.ComponentManagerStatusCode
		id           string
		action       corev1.ComponentResetAction
		completed    bool
		wantError    bool
	}{
		{name: "queued", state: corev1.ComponentResetState_COMPONENT_RESET_STATE_QUEUED},
		{name: "completed", state: corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED, completed: true},
		{name: "failed", state: corev1.ComponentResetState_COMPONENT_RESET_STATE_FAILED, wantError: true},
		{name: "lost job", resultStatus: corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_OUTCOME_UNKNOWN, wantError: true},
		{name: "wrong device", id: "switch-2", state: corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED, wantError: true},
		{name: "wrong action", action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET, state: corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED, wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			id := tc.id
			if id == "" {
				id = "switch-1"
			}
			action := tc.action
			if action == 0 {
				action = corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET
			}
			client := &resetClient{observation: &corev1.GetComponentResetStatusResponse{Statuses: []*corev1.ComponentResetStatus{{Result: &corev1.ComponentResult{ComponentId: &id, Status: tc.resultStatus}, Action: action, State: tc.state}}}}
			completed, err := New(client, nil).NVOSResetCompleted(context.Background(), common.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{"switch-1"}})
			require.Equal(t, tc.completed, completed)
			require.Equal(t, tc.wantError, err != nil)
		})
	}
}

func TestManager_SDNResetCompleted(t *testing.T) {
	for _, tc := range []struct {
		name                 string
		action               corev1.ComponentResetAction
		state                corev1.ComponentResetState
		completed, wantError bool
		reason               string
		resultStatus         corev1.ComponentManagerStatusCode
		wantType             string
	}{
		{name: "SDN completed", action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET, state: corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED, completed: true},
		{name: "SDN failed", reason: "cluster is not enabled\ntarget may not be primary", wantType: "SDNResetFailed", action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET, state: corev1.ComponentResetState_COMPONENT_RESET_STATE_FAILED, wantError: true},
		{name: "lost job preserves observation error", action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET, reason: "RMS job no longer exists", resultStatus: corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_OUTCOME_UNKNOWN, wantError: true},
		{name: "NVOS completion is not SDN completion", action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET, state: corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED, wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &resetClient{observation: &corev1.GetComponentResetStatusResponse{Statuses: []*corev1.ComponentResetStatus{{Result: &corev1.ComponentResult{ComponentId: proto.String("switch-1"), Error: tc.reason, Status: tc.resultStatus}, Action: tc.action, State: tc.state}}}}
			completed, err := New(client, nil).SDNResetCompleted(context.Background(), common.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{"switch-1"}})
			require.Equal(t, tc.completed, completed)
			require.Equal(t, tc.wantError, err != nil)
			if tc.reason != "" {
				require.ErrorContains(t, err, strings.Join(strings.Fields(tc.reason), " "))
			}
			if tc.wantType != "" {
				var applicationError *temporal.ApplicationError
				require.ErrorAs(t, err, &applicationError)
				require.Equal(t, tc.wantType, applicationError.Type())
				require.True(t, applicationError.NonRetryable())
			}
			require.Equal(t, corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET, client.statusRequest.Action)
		})
	}
}

// resetReadinessGate records the batch wait and subsequent bounded recheck.
type resetReadinessGate struct {
	readiness.Gate
	calls   int
	blockAt int
	bounded bool
}

func (g *resetReadinessGate) WaitForRackHostsReady(ctx context.Context, _ []string, _ types.OperationType) error {
	g.calls++
	if g.calls > 1 {
		deadline, ok := ctx.Deadline()
		g.bounded = ok && time.Until(deadline) <= 5*time.Second
	}
	if g.calls == g.blockAt {
		return errors.New("rack hosts in use")
	}
	return nil
}
func (c *resetClient) FindSwitchRackIDs(_ context.Context, ids []string) (map[string]string, error) {
	racks := make(map[string]string, len(ids))
	for _, id := range ids {
		racks[id] = "rack-1"
	}
	return racks, nil
}

func TestManager_SubmitFactoryReset(t *testing.T) {
	t.Run("submission results", checkFactoryResetSubmissionResults)
	for _, tc := range []struct {
		name, scope string
		blockAt     int
		wantCalls   int
		wantError   bool
	}{
		{name: "ready NVOS", scope: "nvos", wantCalls: 1},
		{name: "ready SDN", scope: "sdn", wantCalls: 1},
		{name: "blocked before first mutation", scope: "nvos", blockAt: 1, wantError: true},
		{name: "readiness changes after preflight", scope: "sdn", blockAt: 2, wantError: true},
		{name: "unsupported scope", scope: "bmc", wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &resetClient{response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1")}}}}
			gate := &resetReadinessGate{blockAt: tc.blockAt}
			manager := New(client, gate)
			target := common.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{"switch-1", "switch-2"}}
			err := manager.PrepareFactoryReset(context.Background(), target)
			if err == nil {
				target.Identifiers = target.Identifiers[:1]
				err = manager.SubmitFactoryReset(context.Background(), target, tc.scope)
			}
			require.Equal(t, tc.wantError, err != nil)
			require.Equal(t, tc.wantCalls, client.calls)
			if tc.blockAt != 1 && tc.scope != "bmc" {
				require.True(t, gate.bounded)
			}
			if tc.blockAt == 2 {
				var appErr *temporal.ApplicationError
				require.ErrorAs(t, err, &appErr)
				require.Equal(t, "FactoryResetNotReady", appErr.Type())
			}
			if tc.wantCalls > 0 {
				expected := corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET
				if tc.scope == "sdn" {
					expected = corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET
				}
				require.Equal(t, expected, client.request.Action)
			}
		})
	}
}
