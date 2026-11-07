// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvlinklogicalpartition

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers NVLinkLogicalPartition CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateNVLinkLogicalPartition workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateNVLinkLogicalPartition)
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Successfully registered CreateNVLinkLogicalPartition workflow")

	// Register UpdateNVLinkLogicalPartition workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateNVLinkLogicalPartition)
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Successfully registered UpdateNVLinkLogicalPartition workflow")

	// Register DeleteNVLinkLogicalPartition workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteNVLinkLogicalPartition)
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Successfully registered DeleteNVLinkLogicalPartition workflow")

	// Register activities
	nvLinkLogicalPartitionManager := swa.NewManageNVLinkLogicalPartition(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateNVLinkLogicalPartitionOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(nvLinkLogicalPartitionManager.CreateNVLinkLogicalPartitionOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Successfully registered CreateNVLinkLogicalPartitionOnSite activity")

	// Register UpdateNVLinkLogicalPartitionOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(nvLinkLogicalPartitionManager.UpdateNVLinkLogicalPartitionOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Successfully registered UpdateNVLinkLogicalPartitionOnSite activity")

	// Register DeleteNVLinkLogicalPartitionOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(nvLinkLogicalPartitionManager.DeleteNVLinkLogicalPartitionOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Successfully registered DeleteNVLinkLogicalPartitionOnSite activity")

	return nil
}
