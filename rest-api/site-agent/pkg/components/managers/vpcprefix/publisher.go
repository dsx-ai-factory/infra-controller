// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers VpcPrefix inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Registering inventory workflow and activity")

	// Register DiscoverVpcPrefixInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverVpcPrefixInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered DiscoverVpcPrefixInventory workflow")

	// Register DiscoverVpcPrefixInventory activity
	inventoryManager := swa.NewManageVpcPrefixInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverVpcPrefixInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Successfully registered DiscoverVpcPrefixInventory activity")

	api.RegisterCron()

	return nil
}
