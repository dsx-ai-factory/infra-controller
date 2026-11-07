// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import "fmt"

// Init DpuExtensionService
func (dpuext *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("DpuExtensionService: Initializing DPU Extension Service API")
}

// GetState DpuExtensionService
func (dpuext *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.DpuExtensionServiceState
	var strs []string
	strs = append(strs, fmt.Sprintln("dpuextensionservice_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("dpuextensionservice_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("dpuextensionservice_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("dpuextensionservice_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("dpuextensionservice_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
