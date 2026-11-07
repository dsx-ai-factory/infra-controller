// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvlinklogicalpartition

import "fmt"

// Init NVLinkLogicalPartition
func (nvl *API) Init() {
	// Validate the NVLinkLogicalPartition config later
	ManagerAccess.Data.EB.Log.Info().Msg("NVLinkLogicalPartition: Initializing NVLinkLogicalPartition API")
}

// GetState NVLinkLogicalPartition
func (nvl *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.NVLinkLogicalPartitionState
	var strs []string
	strs = append(strs, fmt.Sprintln("nvlinklogicalpartition_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("nvlinklogicalpartition_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("nvlinklogicalpartition_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("nvlinklogicalpartition_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("nvlinklogicalpartition_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
