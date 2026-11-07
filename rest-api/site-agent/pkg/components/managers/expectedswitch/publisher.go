// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedswitch

import (
	"github.com/google/uuid"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers ExpectedSwitch inventory workflow and activity with Temporal
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Registering inventory workflow and activity")

	// Register DiscoverExpectedSwitchInventory workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverExpectedSwitchInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered DiscoverExpectedSwitchInventory workflow")

	// Register DiscoverExpectedSwitchInventory activity
	inventoryManager := swa.NewManageExpectedSwitchInventory(
		uuid.MustParse(ManagerAccess.Conf.EB.Temporal.ClusterID),
		ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		InventoryCarbidePageSize,
	)

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverExpectedSwitchInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered DiscoverExpectedSwitchInventory activity")

	api.RegisterCron()

	return nil
}
