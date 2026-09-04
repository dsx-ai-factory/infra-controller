// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIDomainCreateRequest is the request body for creating a tenant-owned DNS Domain.
type APIDomainCreateRequest struct {
	Name   string `json:"name"`
	SiteID string `json:"siteId"`
}

// Validate checks the Domain create request before it is sent to Core.
func (dcr APIDomainCreateRequest) Validate() error {
	return validation.ValidateStruct(&dcr,
		validation.Field(&dcr.Name, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&dcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// ToProto converts a validated REST request into Core's Domain create request.
func (dcr APIDomainCreateRequest) ToProto() *corev1.CreateDomainRequest {
	return &corev1.CreateDomainRequest{Name: dcr.Name}
}

// APIDomainGetAllRequest captures optional Domain list filters.
type APIDomainGetAllRequest struct {
	TenantID string `query:"tenantId"`
	SiteID   string `query:"siteId"`
}

// Validate checks optional Domain list filters.
func (dgar APIDomainGetAllRequest) Validate() error {
	return validation.ValidateStruct(&dgar,
		validation.Field(&dgar.TenantID,
			validation.When(dgar.TenantID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&dgar.SiteID,
			validation.When(dgar.SiteID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
	)
}

// APIDomainUpdateRequest is the request body for renaming a tenant-owned DNS Domain.
type APIDomainUpdateRequest struct {
	Name               string    `json:"name"`
	ControllerDomainID uuid.UUID `json:"-"`
}

// Validate checks the Domain update request before it is sent to Core.
func (dur APIDomainUpdateRequest) Validate() error {
	return validation.ValidateStruct(&dur,
		validation.Field(&dur.Name, validation.Required.Error(validationErrorValueRequired)),
	)
}

// ToProto converts a validated REST request into Core's Domain update request.
func (dur APIDomainUpdateRequest) ToProto() *corev1.UpdateDomainRequest {
	return &corev1.UpdateDomainRequest{
		Domain: &corev1.Domain{
			Id:   &corev1.DomainId{Value: dur.ControllerDomainID.String()},
			Name: dur.Name,
		},
	}
}

// APIDomain is the tenant-facing representation of a DNS Domain.
type APIDomain struct {
	ID       string    `json:"id"`
	Name     string    `json:"name"`
	TenantID string    `json:"tenantId"`
	SiteID   string    `json:"siteId"`
	Created  time.Time `json:"created"`
	Updated  time.Time `json:"updated"`
}

// NewAPIDomain converts an owned REST DB projection to its public representation.
func NewAPIDomain(domain *cdbm.Domain) *APIDomain {
	if domain == nil {
		return nil
	}

	siteID := ""
	if domain.SiteID != nil {
		siteID = domain.SiteID.String()
	}
	tenantID := ""
	if domain.TenantID != nil {
		tenantID = domain.TenantID.String()
	}

	return &APIDomain{
		ID:       domain.ID.String(),
		Name:     domain.Hostname,
		TenantID: tenantID,
		SiteID:   siteID,
		Created:  domain.Created,
		Updated:  domain.Updated,
	}
}
