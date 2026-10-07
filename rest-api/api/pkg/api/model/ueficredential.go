// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UEFICredentialKind selects the site-default UEFI credential to create.
type UEFICredentialKind string

const (
	// UEFICredentialKindHost creates the site-default host UEFI credential.
	UEFICredentialKindHost UEFICredentialKind = "Host"
	// UEFICredentialKindDPU creates the site-default DPU UEFI credential.
	UEFICredentialKindDPU UEFICredentialKind = "DPU"
)

// APIUEFICredentialRequest creates a site-default UEFI credential.
type APIUEFICredentialRequest struct {
	// SiteID is the ID of the Site where the credential is stored.
	SiteID string `json:"siteId"`
	// Kind selects the host or DPU UEFI credential.
	Kind UEFICredentialKind `json:"kind"`
	// Password is the credential password.
	Password string `json:"password"`
}

// APIUEFICredential is the UEFI credential response with the password omitted.
type APIUEFICredential struct {
	// SiteID is the ID of the Site where the credential is stored.
	SiteID string `json:"siteId"`
	// Kind identifies the UEFI credential that was stored.
	Kind UEFICredentialKind `json:"kind"`
}

// Validate checks the request before it is converted to a proto.
func (r *APIUEFICredentialRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Kind,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(UEFICredentialKindHost, UEFICredentialKindDPU).Error("invalid kind (expected \"Host\" or \"DPU\")")),
		validation.Field(&r.Password,
			validation.Required.Error("password is required")),
	); err != nil {
		return err
	}
	return nil
}

// ToProto converts the validated request into a CredentialCreationRequest.
func (r *APIUEFICredentialRequest) ToProto() *corev1.CredentialCreationRequest {
	credentialType := corev1.CredentialType_HostUefi
	if r.Kind == UEFICredentialKindDPU {
		credentialType = corev1.CredentialType_DpuUefi
	}
	return &corev1.CredentialCreationRequest{
		CredentialType: credentialType,
		Password:       r.Password,
	}
}

// ToResponse returns the accepted request data without the credential password.
func (r *APIUEFICredentialRequest) ToResponse() *APIUEFICredential {
	return &APIUEFICredential{
		SiteID: r.SiteID,
		Kind:   r.Kind,
	}
}
