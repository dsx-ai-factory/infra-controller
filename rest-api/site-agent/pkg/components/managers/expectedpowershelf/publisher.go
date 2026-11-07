// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers ExpectedPowerShelf inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Registering inventory workflow and activity")

	// Register DiscoverExpectedPowerShelfInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverExpectedPowerShelfInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered DiscoverExpectedPowerShelfInventory workflow")

	// Register DiscoverExpectedPowerShelfInventory activity
	inventoryManager := swa.NewManageExpectedPowerShelfInventory(
		uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		InventoryCarbidePageSize,
	)

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverExpectedPowerShelfInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Successfully registered DiscoverExpectedPowerShelfInventory activity")

	api.RegisterCron()

	return nil
}
