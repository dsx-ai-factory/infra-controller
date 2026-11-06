// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
	"testing"
)

type resetClient struct {
	nicoapi.Client
	response    *corev1.ComponentResetResponse
	observation *corev1.GetComponentResetStatusResponse
	err         error
	request     *corev1.ComponentResetRequest
	calls       int
}

func (c *resetClient) ComponentReset(_ context.Context, req *corev1.ComponentResetRequest) (*corev1.ComponentResetResponse, error) {
	c.calls++
	c.request = req
	return c.response, c.err
}
func (c *resetClient) GetComponentResetStatus(context.Context, *corev1.GetComponentResetStatusRequest) (*corev1.GetComponentResetStatusResponse, error) {
	return c.observation, c.err
}

func TestManager_ResetNVOS(t *testing.T) {
	for _, tc := range []struct {
		name      string
		response  *corev1.ComponentResetResponse
		err       error
		wantError bool
	}{
		{name: "accepted", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1")}}}},
		{name: "lost response", err: status.Error(codes.Unavailable, "response lost"), wantError: true},
		{name: "unsupported Core", err: status.Error(codes.Unimplemented, "unsupported"), wantError: true},
		{name: "missing result", response: &corev1.ComponentResetResponse{}, wantError: true},
		{name: "wrong identity", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-2")}}}, wantError: true},
		{name: "unknown outcome", response: &corev1.ComponentResetResponse{Results: []*corev1.ComponentResult{{ComponentId: proto.String("switch-1"), Status: corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_OUTCOME_UNKNOWN}}}, wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &resetClient{response: tc.response, err: tc.err}
			err := New(client, nil).ResetNVOS(context.Background(), common.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{"switch-1"}})
			require.Equal(t, tc.wantError, err != nil)
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
