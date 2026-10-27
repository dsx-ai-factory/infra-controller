// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	"github.com/google/uuid"
)

// RegisterPublisher registers the SitePrefix inventory workflow, activity, and cron.
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("SitePrefix: Registering inventory workflow and activity")

	// Register DiscoverSitePrefixInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverSitePrefixInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("SitePrefix: Successfully registered DiscoverSitePrefixInventory workflow")

	// Register DiscoverSitePrefixInventory activity
	inventoryManager := swa.NewManageSitePrefixInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverSitePrefixInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("SitePrefix: Successfully registered DiscoverSitePrefixInventory activity")

	return api.RegisterCron()
}
