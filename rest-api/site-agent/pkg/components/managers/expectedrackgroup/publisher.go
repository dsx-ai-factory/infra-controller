// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers ExpectedRackGroup inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Registering inventory workflow and activity")

	// Register DiscoverExpectedRackGroupInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverExpectedRackGroupInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered DiscoverExpectedRackGroupInventory workflow")

	// Register DiscoverExpectedRackGroupInventory activity
	inventoryManager := swa.NewManageExpectedRackGroupInventory(
		uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		InventoryCloudPageSize,
	)

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverExpectedRackGroupInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Successfully registered DiscoverExpectedRackGroupInventory activity")

	return api.RegisterCron()
}
