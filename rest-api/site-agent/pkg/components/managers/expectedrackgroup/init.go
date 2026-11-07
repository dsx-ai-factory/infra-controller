// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

import (
	"fmt"
)

// Init expectedrackgroup
func (er *API) Init() {
	// TODO: validate the expectedrackgroup config.
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedRackGroup: Initializing ExpectedRackGroup API")
}

// GetState - handle http request
func (er *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.ExpectedRackGroupState
	var strs []string
	strs = append(strs, fmt.Sprintln("expectedrackgroup_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("expectedrackgroup_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedrackgroup_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("expectedrackgroup_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedrackgroup_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
