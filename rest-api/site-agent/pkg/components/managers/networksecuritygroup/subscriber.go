// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package networksecuritygroup

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers NetworkSecurityGroup CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateNetworkSecurityGroup workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateNetworkSecurityGroup)
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Successfully registered CreateNetworkSecurityGroup workflow")

	// Register UpdateNetworkSecurityGroup workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateNetworkSecurityGroup)
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Successfully registered UpdateNetworkSecurityGroup workflow")

	// Register DeleteNetworkSecurityGroup workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteNetworkSecurityGroup)
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Successfully registered DeleteNetworkSecurityGroup workflow")

	// Register activities
	networkSecurityGroupManager := swa.NewManageNetworkSecurityGroup(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateNetworkSecurityGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(networkSecurityGroupManager.CreateNetworkSecurityGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Successfully registered CreateNetworkSecurityGroupOnSite activity")

	// Register UpdateNetworkSecurityGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(networkSecurityGroupManager.UpdateNetworkSecurityGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Successfully registered UpdateNetworkSecurityGroupOnSite activity")

	// Register DeleteNetworkSecurityGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(networkSecurityGroupManager.DeleteNetworkSecurityGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Successfully registered DeleteNetworkSecurityGroupOnSite activity")

	return nil
}
