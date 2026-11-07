// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedswitch

import (
	"fmt"
)

// Init expectedswitch
func (es *API) Init() {
	// Validate the expectedswitch config later
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedSwitch: Initializing ExpectedSwitch API")
}

// GetState - handle http request
func (es *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.ExpectedSwitchState
	var strs []string
	strs = append(strs, fmt.Sprintln("expectedswitch_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("expectedswitch_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedswitch_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("expectedswitch_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedswitch_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
