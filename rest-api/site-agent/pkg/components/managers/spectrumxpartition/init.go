// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import "fmt"

// Init SpectrumX Partition manager
func (api *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("SpectrumXPartition: Initializing the SpectrumX Partition manager")
}

// GetState - handle http request
func (api *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.SpectrumXPartitionState
	var strs []string
	strs = append(strs, fmt.Sprintln("spectrumxpartition_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("spectrumxpartition_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("spectrumxpartition_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("spectrumxpartition_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("spectrumxpartition_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
