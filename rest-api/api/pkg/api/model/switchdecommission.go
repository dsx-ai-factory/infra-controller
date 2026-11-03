// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APISwitchDecommissionRequest starts decommissioning one managed switch at a Site.
type APISwitchDecommissionRequest struct {
	// SiteID identifies the Site whose Core inventory contains the component.
	SiteID string `json:"siteId"`
	// SwitchID is the Core inventory ID supplied in the URL.
	SwitchID string `json:"-"`
}

// Validate checks required IDs; Core validates the component ID's hardware-derived format.
func (r *APISwitchDecommissionRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.SwitchID, validation.Required.Error(validationErrorValueRequired)),
	)
}

// ToProto builds the Core decommission request after validation.
func (r *APISwitchDecommissionRequest) ToProto() *corev1.DecommissionSwitchRequest {
	return &corev1.DecommissionSwitchRequest{SwitchId: &corev1.SwitchId{Id: r.SwitchID}}
}
