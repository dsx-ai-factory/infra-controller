// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// APITenantRoutingProfileUpdateRequest changes a Tenant's default at one Site.
type APITenantRoutingProfileUpdateRequest struct {
	SiteID                string `json:"siteId"`
	DefaultRoutingProfile string `json:"defaultRoutingProfile"`
}

// Validate requires a Site and a nonempty configured profile name.
func (r *APITenantRoutingProfileUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required, validationis.UUID),
		validation.Field(&r.DefaultRoutingProfile, validation.Required),
	)
}

// ToProto preserves the observed metadata and guards it with the shared Tenant version.
func (r *APITenantRoutingProfileUpdateRequest) ToProto(tenant *corev1.Tenant) *corev1.UpdateTenantRequest {
	return &corev1.UpdateTenantRequest{
		OrganizationId:     tenant.GetOrganizationId(),
		Metadata:           tenant.GetMetadata(),
		IfVersionMatch:     cutil.GetPtr(tenant.GetVersion()),
		RoutingProfileType: cutil.GetPtr(NormalizeAPIVpcRoutingProfileForSite(r.DefaultRoutingProfile)),
	}
}

// APITenantDefaultRoutingProfile reports the default accepted by the Site.
type APITenantDefaultRoutingProfile struct {
	SiteID                string `json:"siteId"`
	DefaultRoutingProfile string `json:"defaultRoutingProfile"`
}
