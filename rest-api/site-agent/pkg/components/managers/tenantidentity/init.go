// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tenantidentity

import "fmt"

// Init TenantIdentity
func (mi *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("TenantIdentity: Initializing API")
}

// GetState TenantIdentity
func (mi *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.Workflow.TenantIdentityState
	var strs []string
	strs = append(strs, fmt.Sprintln("tenant_identity_workflow_started", state.WflowStarted.Load()))
	strs = append(strs, fmt.Sprintln("tenant_identity_workflow_activity_failed", state.WflowActFail.Load()))
	strs = append(strs, fmt.Sprintln("tenant_identity_workflow_activity_succeeded", state.WflowActSucc.Load()))
	strs = append(strs, fmt.Sprintln("tenant_identity_workflow_publishing_failed", state.WflowPubFail.Load()))
	strs = append(strs, fmt.Sprintln("tenant_identity_workflow_publishing_succeeded", state.WflowPubSucc.Load()))

	return strs
}
