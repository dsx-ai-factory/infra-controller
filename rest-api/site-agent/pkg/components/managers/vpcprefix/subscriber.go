// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers VpcPrefix CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateVpcPrefix workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateVpcPrefix)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered CreateVpcPrefix workflow")

	// Register UpdateVpcPrefix workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateVpcPrefix)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered UpdateVpcPrefix workflow")

	// Register DeleteVpcPrefix workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteVpcPrefix)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered DeleteVpcPrefix workflow")

	// Register activities
	vpcPrefixManager := swa.NewManageVpcPrefix(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateVpcPrefixOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(vpcPrefixManager.CreateVpcPrefixOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered CreateVpcPrefixOnSite activity")

	// Register UpdateVpcPrefixOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(vpcPrefixManager.UpdateVpcPrefixOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered UpdateVpcPrefixOnSite activity")

	// Register DeleteVpcPrefixOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(vpcPrefixManager.DeleteVpcPrefixOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered DeleteVpcPrefixOnSite activity")

	return nil
}
