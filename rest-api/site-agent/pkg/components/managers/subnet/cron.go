// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"context"

	"go.temporal.io/sdk/client"

	wfmgr "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

const (
	// InventoryQueuePrefix is the prefix for the inventory temporal queue
	InventoryQueuePrefix = "inventory-"
	// InventoryCarbidePageSize is the number of items to be fetched from Carbide API at a time
	InventoryCarbidePageSize = 100
	// InventoryCloudPageSize is the number of items to be sent to Cloud at a time
	InventoryCloudPageSize = 25
)

// RegisterCron - Register Cron
func (api *API) RegisterCron() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Subnet: Registering Inventory Discovery Cron")
	workflowID := "inventory-subnet-" + ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace
	cronSchedule := wfmgr.EffectiveCronSchedule()
	ManagerAccess.Data.EB.Log.Info().Str("Schedule", cronSchedule).Msg("Subnet: Inventory Discovery Cron Schedule")

	workflowOptions := client.StartWorkflowOptions{
		ID: workflowID,
		// We would want a separate worker for inventory workflow, for now overload subscriber queue
		// TaskQueue:    InventoryQueuePrefix + ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
		TaskQueue:    ManagerAccess.Conf.EB.Temporal.TemporalSubscribeQueue,
		CronSchedule: cronSchedule,
	}

	we, err := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber.ExecuteWorkflow(
		context.Background(),
		workflowOptions,
		sww.DiscoverSubnetInventory,
	)
	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msg("Subnet: Error registering Inventory Discovery Cron")
	} else {
		ManagerAccess.Data.EB.Log.Info().Interface("workflow Id", we.GetID()).Msg("Subnet: successfully registered the InventoryDiscovery workflow")
	}
	return err
}
