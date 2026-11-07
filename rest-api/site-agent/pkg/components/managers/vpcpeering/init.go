// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcpeering

import "fmt"

// Init  VPC Peering manager
func (api *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("VpcPeering: Initializing the VPC Peering manager")
}

// GetState - handle http request
func (api *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.VpcPeeringState
	var strs []string
	strs = append(strs, fmt.Sprintln("vpcpeering_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("vpcpeering_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("vpcpeering_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("vpcpeering_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("vpcpeering_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
