// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import "fmt"

// Init VpcPrefix
func (sub *API) Init() {
	// Validate the VpcPrefix config later
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPrefix: Initializing the Subnet")
}

// GetState VpcPrefix
func (sub *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.VpcPrefixState
	var strs []string
	strs = append(strs, fmt.Sprintln("vpcprefix_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("vpcprefix_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("vpcprefix_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("vpcprefix_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("vpcprefix_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
