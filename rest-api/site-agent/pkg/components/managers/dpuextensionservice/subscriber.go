// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers DPU Extension Service CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateDpuExtensionService workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateDpuExtensionService)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered CreateDpuExtensionService workflow")

	// Register UpdateDpuExtensionService workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateDpuExtensionService)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered UpdateDpuExtensionService workflow")

	// Register DeleteDpuExtensionService workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteDpuExtensionService)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered DeleteDpuExtensionService workflow")

	// Register GetDpuExtensionServiceVersionsInfo workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.GetDpuExtensionServiceVersionsInfo)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered GetDpuExtensionServiceVersionsInfo workflow")

	// Register activities
	dpuExtServiceManager := swa.NewManageDpuExtensionService(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateDpuExtensionServiceOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(dpuExtServiceManager.CreateDpuExtensionServiceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered CreateDpuExtensionServiceOnSite activity")

	// Register UpdateDpuExtensionServiceOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(dpuExtServiceManager.UpdateDpuExtensionServiceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered UpdateDpuExtensionServiceOnSite activity")

	// Register DeleteDpuExtensionServiceOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(dpuExtServiceManager.DeleteDpuExtensionServiceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered DeleteDpuExtensionServiceOnSite activity")

	// Register GetDpuExtensionServiceVersionsInfoOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(dpuExtServiceManager.GetDpuExtensionServiceVersionsInfoOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered GetDpuExtensionServiceVersionsInfoOnSite activity")

	return nil
}
