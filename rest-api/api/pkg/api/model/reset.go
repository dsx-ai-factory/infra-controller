// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// FactoryResetTargetNVOS identifies the supported factory-reset scope.
const FactoryResetTargetNVOS = "nvos"

// APINVOSFactoryResetRequest selects the site and explicit reset scope for the path's tray or domain.
// There is no readiness override and no automatic restoration after reset.
type APINVOSFactoryResetRequest struct {
	SiteID string `json:"siteId"`
	Target string `json:"target"`
}

func (r *APINVOSFactoryResetRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required, validationis.UUID),
		validation.Field(&r.Target, validation.Required, validation.In(FactoryResetTargetNVOS)),
	)
}

// APINVOSFactoryResetResponse identifies asynchronous tasks, one per owning rack.
type APINVOSFactoryResetResponse struct {
	TaskIDs []string `json:"taskIds"`
}

func (r *APINVOSFactoryResetResponse) FromProto(response *flowv1.SubmitTaskResponse) {
	r.TaskIDs = make([]string, 0, len(response.GetTaskIds()))
	for _, id := range response.GetTaskIds() {
		r.TaskIDs = append(r.TaskIDs, id.GetId())
	}
}
