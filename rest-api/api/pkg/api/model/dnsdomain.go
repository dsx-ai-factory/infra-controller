// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Bounds Core enforces on a Domain's default record TTL, in seconds.
const (
	DNSDomainDefaultTTLMin = 30
	DNSDomainDefaultTTLMax = 86400
)

// APIDNSDomainCreateRequest creates a Domain (DNS zone) on a Site.
type APIDNSDomainCreateRequest struct {
	SiteID string `json:"siteId"`
	Name   string `json:"name"`
	// DefaultTTL is the default record TTL in seconds. Omitted means the Site default.
	DefaultTTL *int `json:"defaultTtl,omitempty"`
}

// APIDNSDomainGetAllRequest lists the Domains on a Site. The json names key
// validation errors by the wire parameter name.
type APIDNSDomainGetAllRequest struct {
	SiteID string `json:"siteId" query:"siteId"`
	// Name filters the list to Domains with exactly this name.
	Name *string `json:"name" query:"name"`
}

// APIDNSDomainGetRequest retrieves one Domain on a Site.
type APIDNSDomainGetRequest struct {
	SiteID string `json:"siteId" query:"siteId"`
	ID     string `json:"id"`
}

// APIDNSDomainUpdateRequest updates a Domain on a Site. The ID comes from the path.
type APIDNSDomainUpdateRequest struct {
	ID     string `json:"id"`
	SiteID string `json:"siteId"`
	// DefaultTTL is the new default record TTL in seconds. Core cannot clear it.
	DefaultTTL *int `json:"defaultTtl"`
}

// APIDNSDomainDeleteRequest deletes a Domain from a Site.
type APIDNSDomainDeleteRequest struct {
	SiteID string `json:"siteId" query:"siteId"`
	ID     string `json:"id"`
}

// APIDNSDomain is a Domain (DNS zone) on a Site.
type APIDNSDomain struct {
	ID     string `json:"id"`
	SiteID string `json:"siteId"`
	Name   string `json:"name"`
	// DefaultTTL is null when the Domain uses the Site default.
	DefaultTTL *int       `json:"defaultTtl"`
	Created    *time.Time `json:"created"`
	Updated    *time.Time `json:"updated"`
}

// Validate checks a Domain create request. Core owns the Domain name rules.
func (r *APIDNSDomainCreateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Name, validation.Required.Error(validationErrorValueRequired)),
		// Min and Max skip a zero value, so a supplied zero is rejected first.
		validation.Field(&r.DefaultTTL,
			validation.NilOrNotEmpty.Error("must be no less than 30"),
			validation.Min(DNSDomainDefaultTTLMin),
			validation.Max(DNSDomainDefaultTTLMax),
		),
	)
}

// Validate checks a Domain list request.
func (r *APIDNSDomainGetAllRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Name, validation.NilOrNotEmpty),
	)
}

// Validate checks a Domain update request. The TTL is the only field Core can
// update, so it is required. Min and Max skip a zero value, so zero is
// rejected before the bounds.
func (r *APIDNSDomainUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.ID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.DefaultTTL,
			validation.NotNil.Error(validationErrorValueRequired),
			validation.NilOrNotEmpty.Error("must be no less than 30"),
			validation.Min(DNSDomainDefaultTTLMin),
			validation.Max(DNSDomainDefaultTTLMax),
		),
	)
}

// Validate checks a Domain get request.
func (r *APIDNSDomainGetRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.ID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// Validate checks a Domain delete request.
func (r *APIDNSDomainDeleteRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.ID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// ToProto converts a validated Domain create request to its Core message.
func (r *APIDNSDomainCreateRequest) ToProto() *corev1.CreateDomainRequest {
	return &corev1.CreateDomainRequest{
		Name:       r.Name,
		DefaultTtl: cutil.IntPtrToUint32Ptr(r.DefaultTTL),
	}
}

// ToProto converts a validated Domain list request to a Core search. Without
// a name the search returns every live Domain on the Site.
func (r *APIDNSDomainGetAllRequest) ToProto() *corev1.DomainSearchQuery {
	return &corev1.DomainSearchQuery{Name: r.Name}
}

// ToProto converts a validated Domain update request to its Core message.
// Core keeps the stored name when the name is empty.
func (r *APIDNSDomainUpdateRequest) ToProto() *corev1.UpdateDomainRequest {
	return &corev1.UpdateDomainRequest{
		Domain: &corev1.Domain{
			Id:         &corev1.DomainId{Value: r.ID},
			DefaultTtl: cutil.IntPtrToUint32Ptr(r.DefaultTTL),
		},
	}
}

// ToProto converts a validated Domain get request to a Core search by ID.
func (r *APIDNSDomainGetRequest) ToProto() *corev1.DomainSearchQuery {
	return &corev1.DomainSearchQuery{Id: &corev1.DomainId{Value: r.ID}}
}

// ToProto converts a validated Domain delete request to its Core message.
func (r *APIDNSDomainDeleteRequest) ToProto() *corev1.DomainDeletionRequest {
	return &corev1.DomainDeletionRequest{Id: &corev1.DomainId{Value: r.ID}}
}

// IsReverseDNSZoneName reports whether name is at or below a reverse-DNS tree
// root. Core keeps a reverse zone row for each aligned Network Segment prefix
// and returns those rows from a Domain search. They are not manageable
// Domains, so the REST API does not expose them.
func IsReverseDNSZoneName(name string) bool {
	normalized := strings.ToLower(strings.TrimRight(strings.TrimSpace(name), "."))
	for _, root := range []string{"in-addr.arpa", "ip6.arpa"} {
		if normalized == root || strings.HasSuffix(normalized, "."+root) {
			return true
		}
	}
	return false
}

// NewAPIDNSDomain creates an API model from a Core Domain on the given Site.
func NewAPIDNSDomain(domain *corev1.Domain, siteID string) *APIDNSDomain {
	if domain == nil {
		return nil
	}
	resp := &APIDNSDomain{}
	resp.FromProto(domain)
	resp.SiteID = siteID
	return resp
}

// FromProto converts one Core Domain. Core does not know the Site ID; the caller sets it.
func (r *APIDNSDomain) FromProto(domain *corev1.Domain) {
	if domain == nil {
		return
	}
	*r = APIDNSDomain{
		ID:         domain.GetId().GetValue(),
		Name:       domain.GetName(),
		DefaultTTL: cutil.Uint32PtrToIntPtr(domain.DefaultTtl),
	}
	if ts := domain.GetCreated(); ts != nil {
		created := ts.AsTime().UTC()
		r.Created = &created
	}
	if ts := domain.GetUpdated(); ts != nil {
		updated := ts.AsTime().UTC()
		r.Updated = &updated
	}
}
