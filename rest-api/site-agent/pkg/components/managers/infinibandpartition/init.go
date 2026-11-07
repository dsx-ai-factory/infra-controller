// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package infinibandpartition

import (
	"fmt"
)

// Init infinibandpartition
func (ibp *API) Init() {
	// Validate the infinibandpartition config later
	ManagerAccess.Data.EB.Log.Info().Msg("InfiniBandPartition: Initializing the InfiniBandPartition")
}

// GetState - handle http request
func (ibp *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.InfiniBandPartitionState
	var strs []string
	strs = append(strs, fmt.Sprintln("infinibandpartition_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("infinibandpartition_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("infinibandpartition_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("infinibandpartition_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("infinibandpartition_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
