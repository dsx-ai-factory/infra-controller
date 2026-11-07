// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

import "fmt"

// Init Operating System
func (OperatingSystem *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Operating System: Initializing the Operating System API")
}

// GetState Operating System
func (OperatingSystem *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.OperatingSystemState
	var strs []string
	strs = append(strs, fmt.Sprintln("operating_system_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("operating_system_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("operating_system_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("operating_system_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("operating_system_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
