// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package networksecuritygroup

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

// RegisterCron - Register cron workflow
func (api *API) RegisterCron() error {
	// Validate the OS Image config later
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Registering Inventory Discovery Cron")

	workflowID := "inventory-network-security-group-" + ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace

	cronSchedule := wfmgr.EffectiveCronSchedule()

	ManagerAccess.Data.EB.Log.Info().Str("Schedule", cronSchedule).Msg("NetworkSecurityGroup: Inventory Discovery Cron Schedule")

	workflowOptions := client.StartWorkflowOptions{
		ID:           workflowID,
		TaskQueue:    ManagerAccess.Conf.EB.Temporal.TemporalSubscribeQueue,
		CronSchedule: cronSchedule,
	}

	we, err := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber.ExecuteWorkflow(
		context.Background(),
		workflowOptions,
		sww.DiscoverNetworkSecurityGroupInventory,
	)

	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msg("NetworkSecurityGroup: Error registering Inventory Collect/Publish Cron")
		return err
	}

	wid := ""
	if !ManagerAccess.Data.EB.Conf.UtMode {
		wid = we.GetID()
	}

	ManagerAccess.Data.EB.Log.Info().Interface("Workflow ID", wid).Msg("NetworkSecurityGroup: successfully registered Inventory Collect/Publish Cron")

	return nil
}
