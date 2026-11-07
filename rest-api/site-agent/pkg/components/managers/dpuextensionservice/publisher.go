// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers DPU Extension Service inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Registering inventory workflow and activity")

	// Register DiscoverDpuExtensionServiceInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverDpuExtensionServiceInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered DiscoverDpuExtensionServiceInventory workflow")

	// Register DiscoverDpuExtensionServiceInventory activity
	dpuExtServiceInventoryManager := swa.NewManageDpuExtensionServiceInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(dpuExtServiceInventoryManager.DiscoverDpuExtensionServiceInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Successfully registered DiscoverDpuExtensionServiceInventory activity")

	api.RegisterCron()

	return nil
}
