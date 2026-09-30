// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIDpuPowerControlQuery identifies the Site that owns the DPU.
type APIDpuPowerControlQuery struct {
	SiteID string `query:"siteId"`
}

func (q *APIDpuPowerControlQuery) Validate() error {
	return validation.ValidateStruct(q,
		validation.Field(&q.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	)
}

// APIDpuPowerControlRequest is deliberately limited to the one DPU power
// operation approved for break-fix automation.
type APIDpuPowerControlRequest struct {
	Action                      MachinePowerAction `json:"action"`
	AcknowledgeAttachedInstance *bool              `json:"acknowledgeAttachedInstance"`
	ExpectedInstanceID          string             `json:"expectedInstanceId,omitempty"`
	ExpectedTenantID            string             `json:"expectedTenantId,omitempty"`
}

func (r *APIDpuPowerControlRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Action,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(MachinePowerActionGracefulRestart).Error("must be GracefulRestart"),
		),
		validation.Field(&r.ExpectedInstanceID, validation.When(r.ExpectedInstanceID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&r.ExpectedTenantID, validation.When(r.ExpectedTenantID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
	)
}

func (r *APIDpuPowerControlRequest) ToProto(dpuMachineID string) *corev1.AdminPowerControlRequest {
	return &corev1.AdminPowerControlRequest{
		MachineId: &dpuMachineID,
		Action:    corev1.AdminPowerControlRequest_GracefulRestart,
	}
}
