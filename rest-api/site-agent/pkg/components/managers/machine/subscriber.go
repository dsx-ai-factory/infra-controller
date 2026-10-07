// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers Machine CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Registering CRUD workflows and activities")

	// Register workflows

	// Register SetMachineMaintenance workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.SetMachineMaintenance)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Successfully registered SetMachineMaintenance workflow")

	// Register UpdateMachineMetadata workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateMachineMetadata)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Successfully registered UpdateMachineMetadata workflow")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateMachineHealthReport)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: successfully registered the CreateMachineHealthReport workflow")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteMachineHealthReport)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: successfully registered the DeleteMachineHealthReport workflow")

	// Register GetDpuMachines workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.GetDpuMachines)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Successfully registered GetDpuMachines workflow")

	// Register activities
	machineManager := swa.NewManageMachine(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register SetMachineMaintenanceOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineManager.SetMachineMaintenanceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Successfully registered SetMachineMaintenanceOnSite activity")

	// Register UpdateMachineMetadataOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineManager.UpdateMachineMetadataOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Successfully registered UpdateMachineMetadataOnSite activity")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineManager.CreateMachineHealthReportOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: successfully registered the CreateMachineHealthReportOnSite activity")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineManager.DeleteMachineHealthReportOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: successfully registered the DeleteMachineHealthReportOnSite activity")

	// Register GetDpuMachinesByIDs activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(machineManager.GetDpuMachinesByIDs)
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Successfully registered GetDpuMachinesByIDs activity")

	return nil
}
