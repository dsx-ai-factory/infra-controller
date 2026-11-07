// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers OperatingSystem inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Registering inventory workflow and activity")

	// Register DiscoverOsImageInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverOsImageInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DiscoverOsImageInventory workflow")

	// Register DiscoverOsImageInventory activity
	osImageInventoryManager := swa.NewManageOsImageInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(osImageInventoryManager.DiscoverOsImageInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DiscoverOsImageInventory activity")

	// Register DiscoverOperatingSystemInventory workflow + activity (iPXE / Templated
	// iPXE OS definitions collected from nico-core and published to the cloud).
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverOperatingSystemInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DiscoverOperatingSystemInventory workflow")

	operatingSystemInventoryManager := swa.NewManageOperatingSystemInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(operatingSystemInventoryManager.DiscoverOperatingSystemInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DiscoverOperatingSystemInventory activity")

	// Register DiscoverIpxeTemplateInventory workflow + activity (PUBLIC iPXE templates
	// collected from nico-core and published to the cloud).
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverIpxeTemplateInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DiscoverIpxeTemplateInventory workflow")

	ipxeTemplateInventoryManager := swa.NewManageIpxeTemplateInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(ipxeTemplateInventoryManager.DiscoverIpxeTemplateInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("OperatingSystem: Successfully registered DiscoverIpxeTemplateInventory activity")

	return api.RegisterCron()
}
