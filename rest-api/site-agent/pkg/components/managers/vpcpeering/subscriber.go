// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcpeering

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers VPC Peering CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Registering CRUD workflows and activities")

	// Register workflows
	// Register CreateVpcPeering workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateVpcPeering)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Successfully registered CreateVpcPeering workflow")

	// Register DeleteVpcPeering workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteVpcPeering)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Successfully registered DeleteVpcPeering workflow")

	// Register activities
	vpcPeeringManager := swa.NewManageVpcPeering(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateVpcPeeringOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(vpcPeeringManager.CreateVpcPeeringOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Successfully registered CreateVpcPeeringOnSite activity")

	// Register DeleteVpcPeeringOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(vpcPeeringManager.DeleteVpcPeeringOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Successfully registered DeleteVpcPeeringOnSite activity")

	return nil
}
