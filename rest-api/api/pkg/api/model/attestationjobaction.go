// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIAttestationJobActionRequest asks Core to waive or fail every retained
// device of one attestation job.
type APIAttestationJobActionRequest struct {
	// SiteID is the Site holding the job
	SiteID string `json:"siteId"`
	// JobID comes from the request path
	JobID string `json:"-"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationJobActionRequest) Validate() error {
	return validation.Errors{
		"siteId": validation.Validate(r.SiteID, requiredUUIDRules...),
		"jobId":  validation.Validate(r.JobID, requiredUUIDRules...),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIAttestationJobActionRequest) ToProto() *corev1.UUID {
	return &corev1.UUID{Value: r.JobID}
}
