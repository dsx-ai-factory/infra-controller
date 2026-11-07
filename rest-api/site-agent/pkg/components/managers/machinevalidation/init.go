// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machinevalidation

import "fmt"

// Init MachineValidation
func (MachineValidation *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("MachineValidation: Initializing API")
}

// GetState MachineValidation
func (MachineValidation *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.MachineValidationState
	var strs []string
	strs = append(strs, fmt.Sprintln("machine_validation_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("machine_validation_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("machine_validation_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("machine_validation_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("machine_validation_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
