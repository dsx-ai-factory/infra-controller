// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"google.golang.org/protobuf/proto"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APITrayDecommissionRequest starts decommissioning one managed tray at a Site.
type APITrayDecommissionRequest struct {
	// SiteID identifies the Site whose Core inventory contains the component.
	SiteID string `json:"siteId"`
	// Type selects the managed component: Compute, NVSwitch, or PowerShelf.
	Type string `json:"type"`
	// TrayID is the Core inventory ID supplied in the URL.
	TrayID string `json:"-"`
}

// Validate checks required IDs; Core validates the component ID's hardware-derived format.
func (r *APITrayDecommissionRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Type, validation.Required.Error(validationErrorValueRequired), validation.In(validTrayTypesAny...)),
		validation.Field(&r.TrayID, validation.Required.Error(validationErrorValueRequired)),
	)
}

// ToProto builds the Core decommission request after validation.
func (r *APITrayDecommissionRequest) ToProto() proto.Message {
	switch r.Type {
	case "Compute":
		return &corev1.DecommissionManagedHostRequest{MachineId: &corev1.MachineId{Id: r.TrayID}}
	case "NVSwitch":
		return &corev1.DecommissionSwitchRequest{SwitchId: &corev1.SwitchId{Id: r.TrayID}}
	case "PowerShelf":
		return &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: r.TrayID}}
	default:
		// Validate rejects unsupported types before conversion.
		return nil
	}
}
