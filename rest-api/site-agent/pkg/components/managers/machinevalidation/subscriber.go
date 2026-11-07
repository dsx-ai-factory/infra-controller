// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machinevalidation

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers MachineValidation CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Registering CRUD workflows and activities")

	// Register workflows

	// Register EnableDisableMachineValidationTest workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.EnableDisableMachineValidationTest)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered EnableDisableMachineValidationTest workflow")

	// Register PersistValidationResult workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.PersistValidationResult)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered PersistValidationResult workflow")

	// Register GetMachineValidationResults workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.GetMachineValidationResults)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationResults workflow")

	// Register GetMachineValidationRuns workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.GetMachineValidationRuns)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationRuns workflow")

	// Register GetMachineValidationTests workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.GetMachineValidationTests)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationTests workflow")

	// Register AddMachineValidationTest workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.AddMachineValidationTest)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered AddMachineValidationTest workflow")

	// Register UpdateMachineValidationTest workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateMachineValidationTest)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered UpdateMachineValidationTest workflow")

	// Register GetMachineValidationExternalConfigs workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.GetMachineValidationExternalConfigs)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationExternalConfigs workflow")

	// Register AddUpdateMachineValidationExternalConfig workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.AddUpdateMachineValidationExternalConfig)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered AddUpdateMachineValidationExternalConfig workflow")

	// Register RemoveMachineValidationExternalConfig workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.RemoveMachineValidationExternalConfig)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered RemoveMachineValidationExternalConfig workflow")

	// Register activities
	machineValidationManager := swa.NewManageMachineValidation(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register EnableDisableMachineValidationTestOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.EnableDisableMachineValidationTestOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered EnableDisableMachineValidationTestOnSite activity")

	// Register PersistValidationResultOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.PersistValidationResultOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered PersistValidationResultOnSite activity")

	// Register GetMachineValidationResultsFromSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.GetMachineValidationResultsFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationResultsFromSite activity")

	// Register GetMachineValidationRunsFromSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.GetMachineValidationRunsFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationRunsFromSite activity")

	// Register GetMachineValidationTestsFromSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.GetMachineValidationTestsFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationTestsFromSite activity")

	// Register AddMachineValidationTestOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.AddMachineValidationTestOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered AddMachineValidationTestOnSite activity")

	// Register UpdateMachineValidationTestOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.UpdateMachineValidationTestOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered UpdateMachineValidationTestOnSite activity")

	// Register GetMachineValidationExternalConfigsFromSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.GetMachineValidationExternalConfigsFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered GetMachineValidationExternalConfigsFromSite activity")

	// Register AddUpdateMachineValidationExternalConfigOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.AddUpdateMachineValidationExternalConfigOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered AddUpdateMachineValidationExternalConfigOnSite activity")

	// Register RemoveMachineValidationExternalConfigOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineValidationManager.RemoveMachineValidationExternalConfigOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Successfully registered RemoveMachineValidationExternalConfigOnSite activity")

	return nil
}
