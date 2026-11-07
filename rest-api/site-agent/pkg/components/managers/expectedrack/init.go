// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	"fmt"
)

// Init expectedrack
func (er *API) Init() {
	// TODO: validate the expectedrack config.
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRack: Initializing ExpectedRack API")
}

// GetState - handle http request
func (er *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.ExpectedRackState
	var strs []string
	strs = append(strs, fmt.Sprintln("expectedrack_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("expectedrack_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedrack_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("expectedrack_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedrack_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
