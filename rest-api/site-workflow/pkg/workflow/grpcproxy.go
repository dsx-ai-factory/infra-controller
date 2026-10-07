// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

// InvokeCoreGRPC is the generic workflow that proxies one already-built NICo
// Core (forge.Forge) gRPC request on the site. It replaces per-method
// workflow/activity pairs: the cloud handler validates and builds each typed
// request, and this workflow forwards one proxy invocation to the site activity
// that holds the Core connection.
//
// The function name is the Temporal workflow type and must match
// grpcproxy.Core.WorkflowName.
func InvokeCoreGRPC(ctx workflow.Context, req grpcproxy.Request) (grpcproxy.Response, error) {
	var manager activity.ManageCoreProxy
	return invokeGRPCProxy(ctx, grpcproxy.Core, manager.InvokeCoreGRPCOnSite, "InvokeCoreGRPCOnSite", req)
}

// InvokeFlowGRPC is the generic workflow that proxies one already-built Flow
// (v1.Flow) gRPC request on the site. It replaces per-method workflow/activity
// pairs: the cloud handler validates and builds each typed request, chooses the
// Temporal workflow ID and conflict policy, and this workflow forwards one
// proxy invocation to the site activity that holds the Flow connection.
//
// The function name is the Temporal workflow type and must match
// grpcproxy.Flow.WorkflowName.
func InvokeFlowGRPC(ctx workflow.Context, req grpcproxy.Request) (grpcproxy.Response, error) {
	var manager activity.ManageFlowProxy
	return invokeGRPCProxy(ctx, grpcproxy.Flow, manager.InvokeFlowGRPCOnSite, "InvokeFlowGRPCOnSite", req)
}

// invokeGRPCProxy runs one proxy activity and translates its result. Both
// backends share it; only the registered workflow and activity names differ.
func invokeGRPCProxy(
	ctx workflow.Context,
	backend grpcproxy.Backend,
	activityFn any,
	activityName string,
	req grpcproxy.Request,
) (grpcproxy.Response, error) {
	logger := log.With().Str("Workflow", backend.WorkflowName).Str("Method", req.FullMethod).Logger()
	logger.Info().Msg("Starting workflow")

	// No automatic retries: a proxied call may be a non-idempotent mutation, so
	// the activity runs exactly once and the caller decides whether to retry.
	options := workflow.ActivityOptions{
		StartToCloseTimeout: grpcproxy.ActivityStartToCloseTimeout,
		RetryPolicy: &temporal.RetryPolicy{
			MaximumAttempts: 1,
		},
	}
	ctx = workflow.WithActivityOptions(ctx, options)

	var resp grpcproxy.Response
	err := workflow.ExecuteActivity(ctx, activityFn, req).Get(ctx, &resp)
	if err != nil {
		logger.Error().Err(err).Str("Activity", activityName).Msg("Failed to execute activity from workflow")
		return grpcproxy.Response{}, err
	}

	logger.Info().Msg("Completing workflow")
	return resp, nil
}
