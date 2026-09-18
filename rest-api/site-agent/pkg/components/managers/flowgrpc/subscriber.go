// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpc

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers the generic Flow gRPC proxy with Temporal.
func (flowgrpc *API) RegisterSubscriber() error {
	if !ManagerAccess.Conf.EB.FlowGrpc.Enabled {
		ManagerAccess.Data.EB.Log.Info().Msg("FlowGrpc: Flow gRPC is disabled, skipping workflow registration")
		return nil
	}

	ManagerAccess.Data.EB.Log.Info().Msg("FlowGrpc: Registering generic Flow gRPC proxy workflow and activity")
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.InvokeFlowGRPC)
	flowProxyManager := swa.NewManageFlowProxy(
		ManagerAccess.Data.EB.Managers.FlowGrpc.Client,
		ManagerAccess.Conf.EB.Temporal.ClusterID,
	)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(flowProxyManager.InvokeFlowGRPCOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("FlowGrpc: Successfully registered InvokeFlowGRPC workflow and activity")

	return nil
}
