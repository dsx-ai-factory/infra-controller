// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers ExpectedMachine CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateExpectedMachine workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateExpectedMachine)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered CreateExpectedMachine workflow")

	// Register UpdateExpectedMachine workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateExpectedMachine)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered UpdateExpectedMachine workflow")

	// Register DeleteExpectedMachine workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteExpectedMachine)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered DeleteExpectedMachine workflow")

	// Register CreateExpectedMachines workflow (Batch)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateExpectedMachines)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered CreateExpectedMachines workflow")

	// Register UpdateExpectedMachines workflow (Batch)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateExpectedMachines)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered UpdateExpectedMachines workflow")

	// Register activities
	expectedMachineManager := swa.NewManageExpectedMachine(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateExpectedMachineOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.CreateExpectedMachineOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered CreateExpectedMachineOnSite activity")

	// Register CreateExpectedMachineOnFlow activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.CreateExpectedMachineOnFlow)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered CreateExpectedMachineOnFlow activity")

	// Register UpdateExpectedMachineOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.UpdateExpectedMachineOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered UpdateExpectedMachineOnSite activity")

	// Register DeleteExpectedMachineOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.DeleteExpectedMachineOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered DeleteExpectedMachineOnSite activity")

	// Register CreateExpectedMachinesOnSite activity (Batch)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered CreateExpectedMachinesOnSite activity")

	// Register CreateExpectedMachinesOnFlow activity (Batch)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnFlow)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered CreateExpectedMachinesOnFlow activity")

	// Register UpdateExpectedMachinesOnSite activity (Batch)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedMachineManager.UpdateExpectedMachinesOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered UpdateExpectedMachinesOnSite activity")

	return nil
}
