// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// BMC credential kinds exposed by the REST API. These map to the admin CLI
// `credential add-bmc --kind=...` surface.
const (
	// BMCCredentialKindSiteWideRoot stores the site-wide BMC root credential
	// (empty username). Maps to CredentialType_SiteWideBmcRoot.
	BMCCredentialKindSiteWideRoot = "SiteWideRoot"
	// BMCCredentialKindBMCRoot stores a per-BMC root credential keyed by MAC
	// address. Maps to CredentialType_RootBmcByMacAddress.
	BMCCredentialKindBMCRoot = "BMCRoot"
)

// APIBMCCredentialRequest sets (creates or overwrites) a BMC credential.
type APIBMCCredentialRequest struct {
	// SiteID is the ID of the Site where the credential is stored.
	SiteID string `json:"siteId"`
	// Kind selects which BMC credential to store: "SiteWideRoot" or "BMCRoot".
	Kind string `json:"kind"`
	// Password is the credential password (required).
	Password string `json:"password"`
	// Username is optional; Core defaults to "root" when omitted for BMCRoot.
	Username *string `json:"username,omitempty"`
	// MacAddress is required for kind "BMCRoot" and ignored for "SiteWideRoot".
	MacAddress *string `json:"macAddress,omitempty"`
}

// APIBMCCredential is the BMC credential response with secret fields omitted.
type APIBMCCredential struct {
	// SiteID is the ID of the Site where the credential is stored.
	SiteID string `json:"siteId"`
	// Kind selects which BMC credential was stored: "SiteWideRoot" or "BMCRoot".
	Kind string `json:"kind"`
	// Username is optional; Core defaults to "root" when omitted for BMCRoot.
	Username *string `json:"username,omitempty"`
	// MacAddress is required for kind "BMCRoot" and ignored for "SiteWideRoot".
	MacAddress *string `json:"macAddress,omitempty"`
}

// Validate checks the request shape before it is converted to a proto.
func (r *APIBMCCredentialRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Kind,
			validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&r.Password,
			validation.Required.Error("password is required")),
	); err != nil {
		return err
	}
	if err := validateBMCCredentialKind(r.Kind, r.MacAddress); err != nil {
		return err
	}
	return nil
}

// ToProto converts the validated request into the forge.Forge
// CredentialCreationRequest.
func (r *APIBMCCredentialRequest) ToProto() *corev1.CredentialCreationRequest {
	return &corev1.CredentialCreationRequest{
		CredentialType: bmcCredentialTypeForKind(r.Kind),
		Password:       r.Password,
		Username:       r.Username,
		MacAddress:     r.MacAddress,
	}
}

// ToResponse returns the accepted request data without the credential password.
func (r *APIBMCCredentialRequest) ToResponse() *APIBMCCredential {
	return &APIBMCCredential{
		SiteID:     r.SiteID,
		Kind:       r.Kind,
		Username:   r.Username,
		MacAddress: r.MacAddress,
	}
}

func validateBMCCredentialKind(kind string, macAddress *string) error {
	switch kind {
	case BMCCredentialKindSiteWideRoot:
		return nil
	case BMCCredentialKindBMCRoot:
		if macAddress == nil || *macAddress == "" {
			return fmt.Errorf("macAddress is required for kind %q", BMCCredentialKindBMCRoot)
		}
		return nil
	default:
		return fmt.Errorf("invalid kind %q (expected %q or %q)", kind, BMCCredentialKindSiteWideRoot, BMCCredentialKindBMCRoot)
	}
}

func bmcCredentialTypeForKind(kind string) corev1.CredentialType {
	switch kind {
	case BMCCredentialKindBMCRoot:
		return corev1.CredentialType_RootBmcByMacAddress
	default:
		return corev1.CredentialType_SiteWideBmcRoot
	}
}
