// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers ExpectedRack inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Registering inventory workflow and activity")

	// Register DiscoverExpectedRackInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverExpectedRackInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered DiscoverExpectedRackInventory workflow")

	// Register DiscoverExpectedRackInventory activity
	inventoryManager := swa.NewManageExpectedRackInventory(
		uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		InventoryCloudPageSize,
	)

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverExpectedRackInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Successfully registered DiscoverExpectedRackInventory activity")

	return api.RegisterCron()
}
