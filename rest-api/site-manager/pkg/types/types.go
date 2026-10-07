// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package types defines api structure types
package types

// SiteCreateRequest defines a site create request
type SiteCreateRequest struct {
	// SiteUUID is the uuid for the site
	SiteUUID string `json:"siteuuid,omitempty"`

	// Name is an optional name for the site
	Name string `json:"name,omitempty"`

	// Provider is the name of the service provider the site belongs to
	Provider string `json:"provider,omitempty"`

	// FCOrg is the name of the Fleetcommand org corresponding to
	// the service provider
	FCOrg string `json:"fcorg,omitempty"`
}

// SiteGetResponse defines a site get response. Empty fields are returned as empty strings.
type SiteGetResponse struct {
	// SiteUUID is the uuid for the site
	SiteUUID string `json:"siteuuid"`

	// Name is an optional name for the site
	Name string `json:"name"`

	// Provider is the name of the service provider the site belongs to
	Provider string `json:"provider"`

	// FCOrg is the name of the Fleetcommand org corresponding to
	// the service provider
	FCOrg string `json:"fcorg"`

	// BootstrapState is the current bootstrap state of the site
	BootstrapState string `json:"bootstrapstate"`

	// ControlPlaneStatus is the current status of the site control plane
	ControlPlaneStatus string `json:"controlplanestatus"`

	// OTP is the current one time passcode
	OTP string `json:"otp"`

	// OTPExpiry is the expiry timestamp
	OTPExpiry string `json:"otpexpiry"`
}

// SiteCredsRequest defines a site credentials request
type SiteCredsRequest struct {
	// SiteUUID is the uuid for the site
	SiteUUID string `json:"siteuuid,omitempty"`
	// OTP is the one time passcode
	OTP string `json:"otp,omitempty"`
}

// SiteCredsResponse defines a site credentials response
type SiteCredsResponse struct {
	// Key is the private key
	Key string `json:"key,omitempty"`
	// Certificate is the client certificate
	Certificate string `json:"certificate,omitempty"`
	// CACertificate is the CA cert for validating the server
	CACertificate string `json:"cacertificate,omitempty"`
}
