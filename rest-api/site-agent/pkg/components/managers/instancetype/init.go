// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instancetype

import "fmt"

// Init initializes Instance Type Manager
func (api *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("InstanceType: Initializing the Subnet")
}

// GetState returns the state of Instance Type Manager
func (api *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.InstanceTypeState
	var strs []string
	strs = append(strs, fmt.Sprintln("instancetype_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("instancetype_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("instancetype_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("instancetype_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("instancetype_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
