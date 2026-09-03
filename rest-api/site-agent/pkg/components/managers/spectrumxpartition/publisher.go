// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	"github.com/google/uuid"
)

// RegisterPublisher registers SpectrumX Partition inventory workflow and activity with Temporal.
//
// There is no matching RegisterSubscriber because SpectrumX Partition create and delete reach
// Core through the generic gRPC proxy rather than per-Site CRUD workflows, so this component
// only collects inventory.
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("SpectrumXPartition: Registering inventory workflow and activity")

	// Register DiscoverSpectrumXPartitionInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverSpectrumXPartitionInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("SpectrumXPartition: Successfully registered DiscoverSpectrumXPartitionInventory workflow")

	// Register DiscoverSpectrumXPartitionInventory activity
	inventoryManager := swa.NewManageSpectrumXPartitionInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverSpectrumXPartitionInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("SpectrumXPartition: Successfully registered DiscoverSpectrumXPartitionInventory activity")

	api.RegisterCron()

	return nil
}
