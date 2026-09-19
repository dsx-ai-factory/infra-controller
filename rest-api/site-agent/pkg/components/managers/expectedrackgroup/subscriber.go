// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers ExpectedRackGroup CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateExpectedRackGroup workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateExpectedRackGroup)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered CreateExpectedRackGroup workflow")

	// Register UpdateExpectedRackGroup workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateExpectedRackGroup)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered UpdateExpectedRackGroup workflow")

	// Register DeleteExpectedRackGroup workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteExpectedRackGroup)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered DeleteExpectedRackGroup workflow")

	// Register ReplaceAllExpectedRackGroups workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.ReplaceAllExpectedRackGroups)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered ReplaceAllExpectedRackGroups workflow")

	// Register DeleteAllExpectedRackGroups workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteAllExpectedRackGroups)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered DeleteAllExpectedRackGroups workflow")

	// Register activities
	expectedRackGroupManager := swa.NewManageExpectedRackGroup(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateExpectedRackGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackGroupManager.CreateExpectedRackGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered CreateExpectedRackGroupOnSite activity")

	// Register UpdateExpectedRackGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackGroupManager.UpdateExpectedRackGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered UpdateExpectedRackGroupOnSite activity")

	// Register DeleteExpectedRackGroupOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackGroupManager.DeleteExpectedRackGroupOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered DeleteExpectedRackGroupOnSite activity")

	// Register ReplaceAllExpectedRackGroupsOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackGroupManager.ReplaceAllExpectedRackGroupsOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered ReplaceAllExpectedRackGroupsOnSite activity")

	// Register DeleteAllExpectedRackGroupsOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackGroupManager.DeleteAllExpectedRackGroupsOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered DeleteAllExpectedRackGroupsOnSite activity")

	return nil
}
