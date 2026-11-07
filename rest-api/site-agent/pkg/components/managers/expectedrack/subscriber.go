// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers ExpectedRack CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateExpectedRack workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateExpectedRack)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered CreateExpectedRack workflow")

	// Register UpdateExpectedRack workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateExpectedRack)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered UpdateExpectedRack workflow")

	// Register DeleteExpectedRack workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteExpectedRack)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered DeleteExpectedRack workflow")

	// Register ReplaceAllExpectedRacks workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.ReplaceAllExpectedRacks)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered ReplaceAllExpectedRacks workflow")

	// Register DeleteAllExpectedRacks workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteAllExpectedRacks)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered DeleteAllExpectedRacks workflow")

	// Register activities
	expectedRackManager := swa.NewManageExpectedRack(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateExpectedRackOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackManager.CreateExpectedRackOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered CreateExpectedRackOnSite activity")

	// Register CreateExpectedRackOnFlow activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackManager.CreateExpectedRackOnFlow)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered CreateExpectedRackOnFlow activity")

	// Register UpdateExpectedRackOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackManager.UpdateExpectedRackOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered UpdateExpectedRackOnSite activity")

	// Register DeleteExpectedRackOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackManager.DeleteExpectedRackOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered DeleteExpectedRackOnSite activity")

	// Register ReplaceAllExpectedRacksOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackManager.ReplaceAllExpectedRacksOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered ReplaceAllExpectedRacksOnSite activity")

	// Register DeleteAllExpectedRacksOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedRackManager.DeleteAllExpectedRacksOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered DeleteAllExpectedRacksOnSite activity")

	return nil
}
