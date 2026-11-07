// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tenant

import "fmt"

// Init Tenant
func (tenant *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Tenant: Initializing Tenant API")
}

// GetState Tenant
func (tenant *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.TenantState
	var strs []string
	strs = append(strs, fmt.Sprintln("tenant_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("tenant_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("tenant_worflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("tenant_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("tenant_worflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
