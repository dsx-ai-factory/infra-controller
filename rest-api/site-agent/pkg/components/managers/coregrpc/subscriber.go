// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers the generic Core gRPC proxy workflow and
// activity with Temporal. This pair can serve any curated REST handler that
// dispatches a Core call through the proxy, avoiding a dedicated workflow and
// activity for each proxied Core method. It lives on the Core gRPC manager
// because that manager owns the connection the activity uses.
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("CoreGrpc: Registering generic Core gRPC proxy workflow and activity")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.InvokeCoreGRPC)

	coreProxyManager := swa.NewManageCoreProxy(ManagerAccess.Data.EB.Managers.CoreGrpc.Client, ManagerAccess.Conf.EB.Temporal.ClusterID)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(coreProxyManager.InvokeCoreGRPCOnSite)

	ManagerAccess.Data.EB.Log.Info().Msg("CoreGrpc: Successfully registered InvokeCoreGRPC workflow and activity")
	return nil
}
