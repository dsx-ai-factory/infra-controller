// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers SSHKeyGroup CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateSSHKeyGroupV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateSSHKeyGroupV2)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered CreateSSHKeyGroupV2 workflow")

	// Register UpdateSSHKeyGroupV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateSSHKeyGroupV2)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered UpdateSSHKeyGroupV2 workflow")

	// Register DeleteSSHKeyGroupV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteSSHKeyGroupV2)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered DeleteSSHKeyGroupV2 workflow")

	// Register activities
	sshKeyGroupManager := swa.NewManageSSHKeyGroup(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateSSHKeyGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(sshKeyGroupManager.CreateSSHKeyGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered CreateSSHKeyGroupOnSite activity")

	// Register UpdateSSHKeyGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered UpdateSSHKeyGroupOnSite activity")

	// Register DeleteSSHKeyGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(sshKeyGroupManager.DeleteSSHKeyGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered DeleteSSHKeyGroupOnSite activity")

	return nil
}
