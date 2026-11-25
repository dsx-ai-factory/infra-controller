// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dhcpoptionset

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers the DhcpOptionSet inventory workflow and activity with Temporal.
//
// There is no RegisterSubscriber because DhcpOptionSet create, update and delete reach Core
// through the generic gRPC proxy, so this manager only collects inventory.
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("DhcpOptionSet: Registering inventory workflow and activity")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverDhcpOptionSetInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("DhcpOptionSet: Successfully registered DiscoverDhcpOptionSetInventory workflow")

	inventoryManager := swa.NewManageDhcpOptionSetInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverDhcpOptionSetInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("DhcpOptionSet: Successfully registered DiscoverDhcpOptionSetInventory activity")

	return api.RegisterCron()
}
