// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcpeering

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	"github.com/google/uuid"
)

// RegisterPublisher registers VPC Peering inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Registering inventory workflow and activity")

	// Register DiscoverVpcPeeringInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverVpcPeeringInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Successfully registered DiscoverVpcPeeringInventory workflow")

	// Register DiscoverVpcPeeringInventory activity
	inventoryManager := swa.NewManageVpcPeeringInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverVpcPeeringInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Successfully registered DiscoverVpcPeeringInventory activity")

	api.RegisterCron()

	return nil
}
