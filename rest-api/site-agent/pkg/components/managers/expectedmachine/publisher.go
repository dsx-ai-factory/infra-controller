// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers ExpectedMachine inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Registering inventory workflow and activity")

	// Register DiscoverExpectedMachineInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverExpectedMachineInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered DiscoverExpectedMachineInventory workflow")

	// Register DiscoverExpectedMachineInventory activity
	inventoryManager := swa.NewManageExpectedMachineInventory(
		uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		InventoryCarbidePageSize,
	)

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverExpectedMachineInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Successfully registered DiscoverExpectedMachineInventory activity")

	api.RegisterCron()

	return nil
}
