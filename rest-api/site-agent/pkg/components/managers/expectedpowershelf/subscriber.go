// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers ExpectedPowerShelf CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateExpectedPowerShelf workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateExpectedPowerShelf)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered CreateExpectedPowerShelf workflow")

	// Register UpdateExpectedPowerShelf workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateExpectedPowerShelf)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered UpdateExpectedPowerShelf workflow")

	// Register DeleteExpectedPowerShelf workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteExpectedPowerShelf)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered DeleteExpectedPowerShelf workflow")

	// Register activities
	expectedPowerShelfManager := swa.NewManageExpectedPowerShelf(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateExpectedPowerShelfOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered CreateExpectedPowerShelfOnSite activity")

	// Register CreateExpectedPowerShelfOnFlow activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnFlow)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered CreateExpectedPowerShelfOnFlow activity")

	// Register UpdateExpectedPowerShelfOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedPowerShelfManager.UpdateExpectedPowerShelfOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered UpdateExpectedPowerShelfOnSite activity")

	// Register DeleteExpectedPowerShelfOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedPowerShelfManager.DeleteExpectedPowerShelfOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered DeleteExpectedPowerShelfOnSite activity")

	return nil
}
