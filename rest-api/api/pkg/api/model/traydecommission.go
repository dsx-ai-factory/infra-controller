// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"regexp"
	"strings"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"google.golang.org/protobuf/proto"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var trayDecommissionIDNamespaceRegexp = regexp.MustCompile(`^(fm100|sw100|ps100)`)

// APITrayDecommissionRequest starts decommissioning one managed tray at a Site.
type APITrayDecommissionRequest struct {
	// SiteID identifies the Site whose Core inventory contains the component.
	SiteID string `json:"siteId"`
	// TrayID is the Core inventory ID supplied in the URL.
	TrayID string `json:"-"`
}

// Validate checks the Site ID and supported Core ID namespaces.
// Core validates the remainder of the component ID's hardware-derived format.
func (r *APITrayDecommissionRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.TrayID, validation.Required.Error(validationErrorValueRequired), validation.Match(trayDecommissionIDNamespaceRegexp).Error("must use a supported Core ID namespace: fm100, sw100, or ps100")),
	)
}

// ToProto builds the Core decommission request after validation.
func (r *APITrayDecommissionRequest) ToProto() proto.Message {
	switch {
	case strings.HasPrefix(r.TrayID, "fm100"):
		return &corev1.DecommissionManagedHostRequest{MachineId: &corev1.MachineId{Id: r.TrayID}}
	case strings.HasPrefix(r.TrayID, "sw100"):
		return &corev1.DecommissionSwitchRequest{SwitchId: &corev1.SwitchId{Id: r.TrayID}}
	case strings.HasPrefix(r.TrayID, "ps100"):
		return &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: r.TrayID}}
	default:
		// Validate rejects unsupported namespaces before conversion.
		return nil
	}
}
