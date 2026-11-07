// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"fmt"
)

// Init SSHKeyGroup
func (SSHKeyGroup *API) Init() {
	// Validate the sshkeygroup config later
	ManagerAccess.Data.EB.Log.Info().Msg("SSHKeyGroup: Initializing the SSHKeyGroup")
}

// GetState - handle http request
func (SSHKeyGroup *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.SSHKeyGroupState
	var strs []string
	strs = append(strs, fmt.Sprintln("sshkeygroup_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("sshkeygroup_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("sshkeygroup_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("sshkeygroup_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("sshkeygroup_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
