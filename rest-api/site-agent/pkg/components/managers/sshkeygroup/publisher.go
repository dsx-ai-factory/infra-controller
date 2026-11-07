// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers SSHKeyGroup inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Registering inventory workflow and activity")

	// Register DiscoverSSHKeyGroupInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverSSHKeyGroupInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered DiscoverSSHKeyGroupInventory workflow")

	// Register DiscoverSSHKeyGroupInventory activity
	inventoryManager := swa.NewManageSSHKeyGroupInventory(swa.ManageInventoryConfig{
		SiteID:                uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		SitePageSize:          InventoryCarbidePageSize,
		CloudPageSize:         InventoryCloudPageSize,
	})

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverSSHKeyGroupInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Successfully registered DiscoverSSHKeyGroupInventory activity")

	api.RegisterCron()

	return nil
}
