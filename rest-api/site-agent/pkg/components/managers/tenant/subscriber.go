// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tenant

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers Tenant CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Tenant: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateTenant workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateTenant)
	ManagerAccess.Data.EB.Log.Info().Msg("Tenant: Successfully registered CreateTenant workflow")

	// Register UpdateTenant workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateTenant)
	ManagerAccess.Data.EB.Log.Info().Msg("Tenant: Successfully registered UpdateTenant workflow")

	// Register activities
	tenantManager := swa.NewManageTenant(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateTenantOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(tenantManager.CreateTenantOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Tenant: Successfully registered CreateTenantOnSite activity")

	// Register UpdateTenantOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(tenantManager.UpdateTenantOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Tenant: Successfully registered UpdateTenantOnSite activity")

	return nil
}
