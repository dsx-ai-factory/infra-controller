// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpc

import (
	"fmt"
)

// Init VPC
func (VPC *API) Init() {
	// Validate the vpc config later
	ManagerAccess.Data.EB.Log.Info().Msg("VPC: Initializing the VPC")
}

// GetState - handle http request
func (VPC *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.VpcState
	var strs []string
	strs = append(strs, fmt.Sprintln("vpc_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("vpc_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("vpc_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("vpc_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("vpc_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
