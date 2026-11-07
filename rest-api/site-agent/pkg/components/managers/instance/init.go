// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import "fmt"

// Init Instance
func (Instance *API) Init() {
	// Validate the Instance config later
	ManagerAccess.Data.EB.Log.Info().Msg("Instance: Initializing the Instance")
}

// GetState Subnet
func (Instance *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.InstanceState
	var strs []string
	strs = append(strs, fmt.Sprintln("instance_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("instance_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("instance_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("instance_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("instance_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
