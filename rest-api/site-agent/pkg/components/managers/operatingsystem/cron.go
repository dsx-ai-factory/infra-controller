// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

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

// RegisterCron registers the OsImage, OperatingSystem, and iPXE template inventory
// discovery crons.
func (api *API) RegisterCron() error {
	if err := api.registerInventoryCron("OS Image", "inventory-os-image-", sww.DiscoverOsImageInventory); err != nil {
		return err
	}
	if err := api.registerInventoryCron("OperatingSystem", "inventory-operating-system-", sww.DiscoverOperatingSystemInventory); err != nil {
		return err
	}
	return api.registerInventoryCron("iPXE Template", "inventory-ipxe-template-", sww.DiscoverIpxeTemplateInventory)
}

// registerInventoryCron schedules a periodic inventory discovery workflow on the
// subscribe queue.
func (api *API) registerInventoryCron(label, workflowIDPrefix string, workflowFunc interface{}) error {
	ManagerAccess.Data.EB.Log.Info().Msgf("%s: Registering Inventory Collect/Publish cron", label)

	workflowID := workflowIDPrefix + ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace

	cronSchedule := wfmgr.EffectiveCronSchedule()

	ManagerAccess.Data.EB.Log.Info().Str("Schedule", cronSchedule).Msgf("%s: Inventory Collect/Publish cron schedule", label)

	workflowOptions := client.StartWorkflowOptions{
		ID:           workflowID,
		TaskQueue:    ManagerAccess.Conf.EB.Temporal.TemporalSubscribeQueue,
		CronSchedule: cronSchedule,
	}

	we, err := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber.ExecuteWorkflow(
		context.Background(),
		workflowOptions,
		workflowFunc,
	)

	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msgf("%s: Error registering Inventory Collect/Publish cron", label)
		return err
	}

	wid := ""
	if !ManagerAccess.Data.EB.Conf.UtMode {
		wid = we.GetID()
	}

	ManagerAccess.Data.EB.Log.Info().Interface("Workflow ID", wid).Msgf("%s: successfully registered Inventory Collect/Publish cron", label)

	return nil
}
