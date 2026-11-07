// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

import (
	"fmt"
)

// Init sku
func (s *API) Init() {
	// Validate the sku config later
	ManagerAccess.Data.EB.Log.Info().Msg("SKU: Initializing SKU API")
}

// GetState - handle http request
func (s *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.SKUState
	var strs []string
	strs = append(strs, fmt.Sprintln("sku_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("sku_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("sku_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("sku_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("sku_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
