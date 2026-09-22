// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	"context"

	"go.temporal.io/sdk/client"

	wfmgr "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

const (
	// InventoryCarbidePageSize is the number of items to be fetched from Carbide API at a time
	InventoryCarbidePageSize = 100
	// InventoryCloudPageSize is the number of items to be sent to Cloud at a time
	InventoryCloudPageSize = 25
)

// RegisterCron - Register Cron
func (api *API) RegisterCron() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Registering Inventory Discovery Cron")

	workflowID := "inventory-expected-power-shelf-" + ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace

	cronSchedule := wfmgr.EffectiveCronSchedule()

	ManagerAccess.Data.EB.Log.Info().Str("Schedule", cronSchedule).Msg("ExpectedPowerShelf: Inventory Discovery Cron Schedule")

	workflowOptions := client.StartWorkflowOptions{
		ID:           workflowID,
		TaskQueue:    ManagerAccess.Conf.EB.Temporal.TemporalSubscribeQueue,
		CronSchedule: cronSchedule,
	}

	we, err := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber.ExecuteWorkflow(
		context.Background(),
		workflowOptions,
		sww.DiscoverExpectedPowerShelfInventory,
	)
	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msg("ExpectedPowerShelf: Error registering Inventory Discovery Cron")
		return err
	}

	ManagerAccess.Data.EB.Log.Info().Interface("Workflow ID", we.GetID()).Msg("ExpectedPowerShelf: successfully registered Inventory Discovery Cron")

	return nil
}
