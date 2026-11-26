// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIDpuMachinePowerUpdateRequest is the request body for PATCH /dpu/:id/power.
// It accepts the same power actions as APIMachinePowerControlRequest.
type APIDpuMachinePowerUpdateRequest struct {
	// SiteID is the ID of the Site the DPU Machine belongs to
	SiteID string `json:"siteId"`
	// Action is the power control action to perform on the DPU Machine
	Action MachinePowerAction `json:"action"`
	// AcknowledgeAttachedInstance indicates the caller is aware that an Instance is currently attached to the DPU's host Machine
	AcknowledgeAttachedInstance *bool `json:"acknowledgeAttachedInstance"`
	// DpuMachineID is the DPU Machine ID from the URL path, set by the handler
	DpuMachineID string `json:"-"`
}

// Validate checks the DPU Machine power request body.
func (admpur *APIDpuMachinePowerUpdateRequest) Validate() error {
	return validation.ValidateStruct(admpur,
		validation.Field(&admpur.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
		validation.Field(&admpur.Action,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validMachinePowerActionsAny...).Error(fmt.Sprintf("must be one of %v", validMachinePowerActions))),
	)
}

// ToProto converts the request to the Core power control request for the DPU Machine.
func (admpur *APIDpuMachinePowerUpdateRequest) ToProto() *corev1.AdminPowerControlRequest {
	return &corev1.AdminPowerControlRequest{
		MachineId: &admpur.DpuMachineID,
		Action:    admpur.Action.ToProto(),
	}
}
