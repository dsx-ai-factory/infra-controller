// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import "fmt"

// Init Machine
func (Machine *API) Init() {
	// Validate the Machine config later
	ManagerAccess.Data.EB.Log.Info().Msg("Machine: Initializing the Machine")
}

// GetState Machine
func (Machine *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.MachineState
	var strs []string
	strs = append(strs, fmt.Sprintln("machine_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("machine_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("machine_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("machine_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("machine_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
