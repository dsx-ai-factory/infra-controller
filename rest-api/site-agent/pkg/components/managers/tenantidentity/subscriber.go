// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tenantidentity

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers one workflow+activity pair per Forge
// tenant-identity RPC with the Temporal worker.
func (mi *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Registering the subscribers")

	manager := swa.NewManageTenantIdentity(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)
	w := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker

	w.RegisterWorkflow(sww.CreateOrUpdateTenantIdentityConfiguration)
	w.RegisterActivity(manager.CreateOrUpdateTenantIdentityConfigurationOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered CreateOrUpdateTenantIdentityConfiguration workflow & activity")

	w.RegisterWorkflow(sww.GetTenantIdentityConfiguration)
	w.RegisterActivity(manager.GetTenantIdentityConfigurationFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered GetTenantIdentityConfiguration workflow & activity")

	w.RegisterWorkflow(sww.DeleteTenantIdentityConfiguration)
	w.RegisterActivity(manager.DeleteTenantIdentityConfigurationOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered DeleteTenantIdentityConfiguration workflow & activity")

	w.RegisterWorkflow(sww.CreateOrUpdateTenantIdentityTokenDelegation)
	w.RegisterActivity(manager.CreateOrUpdateTenantIdentityTokenDelegationOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered CreateOrUpdateTenantIdentityTokenDelegation workflow & activity")

	w.RegisterWorkflow(sww.GetTenantIdentityTokenDelegation)
	w.RegisterActivity(manager.GetTenantIdentityTokenDelegationFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered GetTenantIdentityTokenDelegation workflow & activity")

	w.RegisterWorkflow(sww.DeleteTenantIdentityTokenDelegation)
	w.RegisterActivity(manager.DeleteTenantIdentityTokenDelegationOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered DeleteTenantIdentityTokenDelegation workflow & activity")

	w.RegisterWorkflow(sww.GetJWKS)
	w.RegisterActivity(manager.GetJWKSFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered GetJWKS workflow & activity")

	w.RegisterWorkflow(sww.GetOpenIDConfiguration)
	w.RegisterActivity(manager.GetOpenIDConfigurationFromSite)
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Successfully registered GetOpenIDConfiguration workflow & activity")

	return nil
}
