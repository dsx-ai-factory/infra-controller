// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedswitch

import (
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterSubscriber registers ExpectedSwitch CRUD workflows and activities with Temporal
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Registering CRUD workflows and activities")

	// Register workflows

	// Register CreateExpectedSwitch workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.CreateExpectedSwitch)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered CreateExpectedSwitch workflow")

	// Register UpdateExpectedSwitch workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.UpdateExpectedSwitch)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered UpdateExpectedSwitch workflow")

	// Register DeleteExpectedSwitch workflow
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DeleteExpectedSwitch)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered DeleteExpectedSwitch workflow")

	// Register activities
	expectedSwitchManager := swa.NewManageExpectedSwitch(ManagerAccess.Data.EB.Managers.CoreGrpc.Client)

	// Register CreateExpectedSwitchOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedSwitchManager.CreateExpectedSwitchOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered CreateExpectedSwitchOnSite activity")

	// Register CreateExpectedSwitchOnFlow activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedSwitchManager.CreateExpectedSwitchOnFlow)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered CreateExpectedSwitchOnFlow activity")

	// Register UpdateExpectedSwitchOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedSwitchManager.UpdateExpectedSwitchOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered UpdateExpectedSwitchOnSite activity")

	// Register DeleteExpectedSwitchOnSite activity
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(expectedSwitchManager.DeleteExpectedSwitchOnSite)
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Successfully registered DeleteExpectedSwitchOnSite activity")

	return nil
}
