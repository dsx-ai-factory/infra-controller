// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	"fmt"
)

// Init expectedpowershelf
func (eps *API) Init() {
	// Validate the expectedpowershelf config later
	ManagerAccess.Data.EB.Log.Info().Msg("ExpectedPowerShelf: Initializing ExpectedPowerShelf API")
}

// GetState - handle http request
func (eps *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.ExpectedPowerShelfState
	var strs []string
	strs = append(strs, fmt.Sprintln("expectedpowershelf_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("expectedpowershelf_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedpowershelf_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("expectedpowershelf_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("expectedpowershelf_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
