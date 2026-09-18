// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"net/http"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	cam "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// proxiedCall is what a mutation helper handed to the proxy workflow.
type proxiedCall struct {
	options      tclient.StartWorkflowOptions
	workflowName string
	request      grpcproxy.Request
}

// newMutationProxyClient returns a Temporal client that captures the proxy call
// and answers it with reply, so a helper runs its success path.
func newMutationProxyClient(t *testing.T, reply proto.Message) (*tmocks.Client, *proxiedCall) {
	t.Helper()

	replyJSON, err := protojson.Marshal(reply)
	require.NoError(t, err)

	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		out, ok := args.Get(1).(*grpcproxy.Response)
		require.True(t, ok, "Get target is %T", args.Get(1))
		out.ResponseJSON = replyJSON
	}).Return(nil)

	call := &proxiedCall{}
	temporalClient := &tmocks.Client{}
	temporalClient.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		call.options = args.Get(1).(tclient.StartWorkflowOptions)
		call.workflowName = args.Get(2).(string)
		call.request = args.Get(3).(grpcproxy.Request)
	}).Return(workflowRun, nil)

	return temporalClient, call
}

func mutationTargetSpec(rackID string) *flowv1.OperationTargetSpec {
	return &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Racks{
			Racks: &flowv1.RackTargets{
				Targets: []*flowv1.RackTarget{
					{Identifier: &flowv1.RackTarget_Id{Id: &flowv1.UUID{Id: rackID}}},
				},
			},
		},
	}
}

// TestFlowMutationHelpersProxyRequests pins what the power, bring-up and
// firmware helpers send. Each covers a Flow method reached only through these
// helpers, so an incorrect method, workflow ID, conflict policy or request
// payload would otherwise surface as a misrouted mutation on a live site.
func TestFlowMutationHelpersProxyRequests(t *testing.T) {
	const (
		rackID     = "6f1b7c4e-9c2a-4d1e-8f3b-2a5c7d9e1b04"
		ruleID     = "b3d2c1a0-5e4f-4a3b-9c8d-7e6f5a4b3c2d"
		siteID     = "58fe7a29-58b7-4d61-9b64-e355eaff6024"
		workflowID = "rack-power-1"
		entityName = "rack r1"
	)
	ruleIDArg := ruleID
	version := "1.2.3"

	cases := []struct {
		name           string
		execute        func(context.Context, echo.Context, tclient.Client) (*flowv1.SubmitTaskResponse, error)
		wantFullMethod string
		wantRequest    proto.Message
	}{
		{
			name: "power on",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecutePowerControlWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), cam.PowerControlStateOn, &ruleIDArg, false, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_PowerOnRack_FullMethodName,
			wantRequest: &flowv1.PowerOnRackRequest{
				TargetSpec:  mutationTargetSpec(rackID),
				Description: "API power on rack r1",
				RuleId:      &flowv1.UUID{Id: ruleID},
			},
		},
		{
			name: "power off",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecutePowerControlWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), cam.PowerControlStateOff, nil, false, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_PowerOffRack_FullMethodName,
			wantRequest: &flowv1.PowerOffRackRequest{
				TargetSpec:  mutationTargetSpec(rackID),
				Description: "API power off rack r1",
			},
		},
		{
			name: "power cycle",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecutePowerControlWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), cam.PowerControlStateCycle, nil, false, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_PowerResetRack_FullMethodName,
			wantRequest: &flowv1.PowerResetRackRequest{
				TargetSpec:  mutationTargetSpec(rackID),
				Description: "API power cycle rack r1",
			},
		},
		{
			// Forced shares PowerOffRack with the unforced state, so the flag is
			// the only thing separating them on the wire.
			name: "force power off",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecutePowerControlWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), cam.PowerControlStateForceOff, nil, true, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_PowerOffRack_FullMethodName,
			wantRequest: &flowv1.PowerOffRackRequest{
				TargetSpec:             mutationTargetSpec(rackID),
				Forced:                 true,
				Description:            "API force power off rack r1",
				OverrideReadinessCheck: true,
			},
		},
		{
			name: "force power cycle",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecutePowerControlWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), cam.PowerControlStateForceCycle, nil, false, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_PowerResetRack_FullMethodName,
			wantRequest: &flowv1.PowerResetRackRequest{
				TargetSpec:  mutationTargetSpec(rackID),
				Forced:      true,
				Description: "API force power cycle rack r1",
			},
		},
		{
			name: "bring up",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecuteBringUpRackWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), "API bring up rack r1", &ruleIDArg, true, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_BringUpRack_FullMethodName,
			wantRequest: &flowv1.BringUpRackRequest{
				TargetSpec:             mutationTargetSpec(rackID),
				Description:            "API bring up rack r1",
				RuleId:                 &flowv1.UUID{Id: ruleID},
				OverrideReadinessCheck: true,
			},
		},
		{
			name: "firmware update",
			execute: func(ctx context.Context, c echo.Context, stc tclient.Client) (*flowv1.SubmitTaskResponse, error) {
				return ExecuteFirmwareUpdateWorkflow(ctx, c, zerolog.Nop(), stc, mutationTargetSpec(rackID), &version, []string{"bmc", "nvos"}, nil, siteID, nil, false, workflowID, entityName)
			},
			wantFullMethod: flowv1.Flow_UpgradeFirmware_FullMethodName,
			wantRequest: &flowv1.UpgradeFirmwareRequest{
				TargetSpec:    mutationTargetSpec(rackID),
				TargetVersion: &version,
				SubTargets:    []string{"bmc", "nvos"},
				Description:   "API firmware update rack r1",
			},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reply := &flowv1.SubmitTaskResponse{}
			temporalClient, call := newMutationProxyClient(t, reply)
			echoCtx, _ := newProxyEchoContext()

			got, err := tc.execute(context.Background(), echoCtx, temporalClient)

			require.NoError(t, err)
			require.NotNil(t, got)

			assert.Equal(t, grpcproxy.Flow.WorkflowName, call.workflowName)
			assert.Equal(t, tc.wantFullMethod, call.request.FullMethod)

			t.Run("coalesces retries onto the mutation already in flight", func(t *testing.T) {
				assert.Equal(t, workflowID, call.options.ID)
				assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, call.options.WorkflowIDConflictPolicy)
			})

			t.Run("sends the expected request", func(t *testing.T) {
				decoded := tc.wantRequest.ProtoReflect().New().Interface()
				require.NoError(t, protojson.Unmarshal(call.request.RequestJSON, decoded))
				assert.Empty(t, call.request.EncryptedSecrets)
				assert.True(t, proto.Equal(tc.wantRequest, decoded), "want %v, got %v", tc.wantRequest, decoded)
			})
		})
	}
}

func TestExecuteFirmwareUpdateWorkflowEncryptsAuthenticationData(t *testing.T) {
	const (
		rackID     = "6f1b7c4e-9c2a-4d1e-8f3b-2a5c7d9e1b04"
		siteID     = "58fe7a29-58b7-4d61-9b64-e355eaff6024"
		workflowID = "rack-firmware-1"
		token      = "firmware-access-token"
	)
	authenticationData := &flowv1.FirmwareAuthenticationData{
		Value: &flowv1.FirmwareAuthenticationData_Shared{Shared: token},
	}
	wantRequest := &flowv1.UpgradeFirmwareRequest{
		TargetSpec:         mutationTargetSpec(rackID),
		Description:        "API firmware update rack r1",
		AuthenticationData: authenticationData,
	}
	temporalClient, call := newMutationProxyClient(t, &flowv1.SubmitTaskResponse{})
	echoCtx, _ := newProxyEchoContext()

	got, err := ExecuteFirmwareUpdateWorkflow(
		context.Background(), echoCtx, zerolog.Nop(), temporalClient,
		mutationTargetSpec(rackID), nil, nil, authenticationData, siteID, nil, false,
		workflowID, "rack r1",
	)

	require.NoError(t, err)
	require.NotNil(t, got)
	assert.True(t, strings.HasPrefix(call.options.ID, workflowID+"-"))
	assert.NotContains(t, call.options.ID, token)
	assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, call.options.WorkflowIDConflictPolicy)
	assert.NotContains(t, string(call.request.RequestJSON), token)
	assert.Contains(t, string(call.request.RequestJSON), grpcproxy.RedactedPlaceholder)
	require.NotEmpty(t, call.request.EncryptedSecrets)

	secretsJSON := cutil.DecryptData(call.request.EncryptedSecrets, siteID)
	restoredJSON, err := grpcproxy.MergeSecrets(call.request.RequestJSON, secretsJSON)
	require.NoError(t, err)
	gotRequest := &flowv1.UpgradeFirmwareRequest{}
	require.NoError(t, protojson.Unmarshal(restoredJSON, gotRequest))
	assert.True(t, proto.Equal(wantRequest, gotRequest), "want %v, got %v", wantRequest, gotRequest)
}

// TestExecutePowerControlWorkflowRejectsUnknownState keeps an unroutable state
// from reaching Flow: the helper picks the method from it, so there is nothing
// to proxy. It answers 400 on the response itself rather than through the
// returned error, which stays nil once the write succeeds.
func TestExecutePowerControlWorkflowRejectsUnknownState(t *testing.T) {
	temporalClient := &tmocks.Client{}
	echoCtx, recorder := newProxyEchoContext()

	got, err := ExecutePowerControlWorkflow(context.Background(), echoCtx, zerolog.Nop(), temporalClient, mutationTargetSpec("6f1b7c4e-9c2a-4d1e-8f3b-2a5c7d9e1b04"), "hibernate", nil, false, "rack-power-1", "rack r1")

	assert.Nil(t, got)
	assert.NoError(t, err)
	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Contains(t, recorder.Body.String(), "Invalid power control state: hibernate")
	temporalClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
}
