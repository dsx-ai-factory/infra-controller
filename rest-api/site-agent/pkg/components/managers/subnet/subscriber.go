// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers Subnet CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Subnet: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateSubnetV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateSubnetV2)
	ManagerAccess.Data.EB.Log.Info().Msg("Subnet: Successfully registered CreateSubnetV2 workflow")

	// Register DeleteSubnetV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteSubnetV2)
	ManagerAccess.Data.EB.Log.Info().Msg("Subnet: Successfully registered DeleteSubnetV2 workflow")

	// Register activities

	subnetManager := swa.NewManageSubnet(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateSubnetOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(subnetManager.CreateSubnetOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Subnet: Successfully registered CreateSubnetOnSite activity")

	// Register DeleteSubnetOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(subnetManager.DeleteSubnetOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Subnet: Successfully registered DeleteSubnetOnSite activity")

	return nil
}
