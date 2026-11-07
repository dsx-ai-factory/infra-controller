// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"

	"go.temporal.io/sdk/client"

	wfmgr "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

const (
	// InventoryQueuePrefix is the prefix for the inventory temporal queue.
	InventoryQueuePrefix = "inventory-"
)

// RegisterCron registers the Site Config inventory discovery cron.
func (api *API) RegisterCron() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Site: Registering Site Config Inventory Discovery Cron")
	workflowID := "inventory-site-config-" + ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace
	cronSchedule := wfmgr.EffectiveCronSchedule()
	ManagerAccess.Data.EB.Log.Info().Str("Schedule", cronSchedule).Msg("Site: Site Config Inventory Discovery Cron Schedule")

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
		sww.DiscoverSiteConfigInventory,
	)
	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msg("Site: Error registering Site Config Inventory Discovery Cron")
	} else {
		ManagerAccess.Data.EB.Log.Info().Interface("workflow Id", we.GetID()).Msg("Site: successfully registered the Site Config Inventory Discovery workflow")
	}
	return err
}
