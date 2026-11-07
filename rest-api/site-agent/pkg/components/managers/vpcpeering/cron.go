// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcpeering

import (
	"context"

	"go.temporal.io/sdk/client"

	wfmgr "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

const (
	// InventoryCarbidePageSize is the number of items to fetch from Carbide API per page
	InventoryCarbidePageSize = 100
	// InventoryCloudPageSize is the number of items to send to cloud per Temporal workflow page
	InventoryCloudPageSize = 25
)

// RegisterCron - register cron
func (api *API) RegisterCron() error {
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Registering Inventory Discovery Cron")

	workflowID := "inventory-vpcpeering-" + ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace

	cronSchedule := wfmgr.EffectiveCronSchedule()

	ManagerAccess.Data.EB.Log.Info().Str("Schedule", cronSchedule).Msg("VpcPeering: Inventory Discovery Cron Schedule")

	workflowOptions := client.StartWorkflowOptions{
		ID:           workflowID,
		TaskQueue:    ManagerAccess.Conf.EB.Temporal.TemporalSubscribeQueue,
		CronSchedule: cronSchedule,
	}

	we, err := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber.ExecuteWorkflow(
		context.Background(),
		workflowOptions,
		sww.DiscoverVpcPeeringInventory,
	)

	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msg("VpcPeering: Error registering Inventory Collect/Publish cron")
		return err
	}

	wid := ""
	if !ManagerAccess.Data.EB.Conf.UtMode {
		wid = we.GetID()
	}

	ManagerAccess.Data.EB.Log.Info().Interface("Workflow ID", wid).Msg("VpcPeering: successfully registered Inventory Collect/Publish cron")

	return nil
}
