// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package networksecuritygroup

import "fmt"

// Init initializes NetworkSecurityGroup Manager
func (api *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("NetworkSecurityGroup: Initializing")
}

// GetState returns the state of NetworkSecurityGroup Manager
func (api *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.NetworkSecurityGroupState
	var strs []string
	strs = append(strs, fmt.Sprintln("networksecuritygroup_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("networksecuritygroup_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("networksecuritygroup_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("networksecuritygroup_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("networksecuritygroup_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
