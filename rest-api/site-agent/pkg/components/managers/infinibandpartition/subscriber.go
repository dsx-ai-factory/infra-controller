// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package infinibandpartition

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers InfiniBandPartition CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateInfiniBandPartitionV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateInfiniBandPartitionV2)
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Successfully registered CreateInfiniBandPartitionV2 workflow")

	// UpdateInfiniBandPartition
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateInfiniBandPartition)
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: successfully registered UpdateInfiniBandPartition workflow")

	// Register DeleteInfiniBandPartitionV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteInfiniBandPartitionV2)
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Successfully registered DeleteInfiniBandPartitionV2 workflow")

	// Register activities
	ibpManager := swa.NewManageInfiniBandPartition(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateInfiniBandPartitionOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(ibpManager.CreateInfiniBandPartitionOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Successfully registered CreateInfiniBandPartitionOnSite activity")

	// Register UpdateInfiniBandPartitionOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(ibpManager.UpdateInfiniBandPartitionOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Successfully registered UpdateInfiniBandPartitionOnSite activity")

	// Register DeleteInfiniBandPartitionOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(ibpManager.DeleteInfiniBandPartitionOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Successfully registered DeleteInfiniBandPartitionOnSite activity")

	return nil
}
