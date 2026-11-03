// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIPowerShelfDecommissionRequest starts decommissioning one managed power shelf at a Site.
type APIPowerShelfDecommissionRequest struct {
	// SiteID identifies the Site whose Core inventory contains the component.
	SiteID string `json:"siteId"`
	// PowerShelfID is the Core inventory ID supplied in the URL.
	PowerShelfID string `json:"-"`
}

// Validate checks required IDs; Core validates the component ID's hardware-derived format.
func (r *APIPowerShelfDecommissionRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.PowerShelfID, validation.Required.Error(validationErrorValueRequired)),
	)
}

// ToProto builds the Core decommission request after validation.
func (r *APIPowerShelfDecommissionRequest) ToProto() *corev1.DecommissionPowerShelfRequest {
	return &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: r.PowerShelfID}}
}
