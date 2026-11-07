// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers OperatingSystem CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateOsImage workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateOsImage)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered CreateOsImage workflow")

	// Register UpdateOsImage workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateOsImage)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered UpdateOsImage workflow")

	// Register DeleteOsImage workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteOsImage)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DeleteOsImage workflow")

	// Register activities
	osImageManager := swa.NewManageOperatingSystem(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateOsImageOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(osImageManager.CreateOsImageOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered CreateOsImageOnSite activity")

	// Register UpdateOsImageOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(osImageManager.UpdateOsImageOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered UpdateOsImageOnSite activity")

	// Register DeleteOsImageOnSite
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(osImageManager.DeleteOsImageOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DeleteOsImageOnSite activity")

	return nil
}
