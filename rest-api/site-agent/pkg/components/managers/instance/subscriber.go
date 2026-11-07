// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"go.temporal.io/sdk/workflow"

	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers Instance CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateInstanceV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateInstanceV2)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered CreateInstanceV2 workflow")

	// Register CreateInstances workflow (Batch)
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateInstances)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered CreateInstances workflow")

	// Register DeleteInstanceV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteInstanceV2)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered DeleteInstanceV2 workflow")

	// Register UpdateInstance workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateInstance)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered UpdateInstance workflow")

	// Register RebootInstanceV2 workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflowWithOptions(sww.RebootInstance, workflow.RegisterOptions{
		Name: "RebootInstanceV2",
	})
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered RebootInstanceV2 workflow")

	// Register activities

	instanceManager := swa.NewManageInstance(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateInstanceOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(instanceManager.CreateInstanceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered CreateInstanceOnSite activity")

	// Register CreateInstancesOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(instanceManager.CreateInstancesOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered CreateInstancesOnSite activity")

	// Register DeleteInstanceOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(instanceManager.DeleteInstanceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered DeleteInstanceOnSite activity")

	// Register UpdateInstanceOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(instanceManager.UpdateInstanceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered UpdateInstanceOnSite activity")

	// Register RebootInstanceOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(instanceManager.RebootInstanceOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Successfully registered RebootInstanceOnSite activity")

	return nil
}
