// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"fmt"
)

// Init expectedmachine
func (em *API) Init() {
	// Validate the expectedmachine config later
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedMachine: Initializing ExpectedMachine API")
}

// GetState - handle http request
func (em *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.ExpectedMachineState
	var strs []string
	strs = append(strs, fmt.Sprintln("expectedmachine_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("expectedmachine_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedmachine_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("expectedmachine_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedmachine_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
