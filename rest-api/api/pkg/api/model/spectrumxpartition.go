// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"math"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// APISpectrumXPartitionCreateRequest is the data structure to capture a user request to create a new SpectrumXPartition
type APISpectrumXPartitionCreateRequest struct {
	// Name is the name of the SpectrumX Partition
	Name string `json:"name"`
	// Description is the description of the SpectrumX Partition
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// VNI is the requested VXLAN Network Identifier. Omit it to let the Site
	// allocate one from its pool. A requested VNI that is already allocated or
	// outside the Site's pool is rejected by the Site.
	VNI *int `json:"vni"`
	// Labels is the labels of the SpectrumX Partition
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (sxpcr *APISpectrumXPartitionCreateRequest) Validate() error {
	err := validation.ValidateStruct(sxpcr,
		validation.Field(&sxpcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&sxpcr.Description,
			validation.When(sxpcr.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&sxpcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		// The Site holds VNIs in a signed 32-bit resource pool, so bound the request
		// to what that pool can represent. Which values are actually free is the
		// Site's to decide, and it rejects an unavailable one.
		validation.Field(&sxpcr.VNI,
			validation.Min(0).Error("value must be equal or greater than 0"),
			validation.Max(math.MaxInt32).Error("value must be less than or equal to 2147483647")),
	)

	if err != nil {
		return err
	}

	return util.ValidateLabels(sxpcr.Labels)
}

// ToProto builds the request that asks a Site to create this SpectrumX
// Partition. `sxp` is the just-persisted DB record; its `ToProto()` is the
// source of the canonical wire fields, which the create request reuses.
//
// A nil `sxp.VNI` leaves the optional wire field unset so the Site allocates
// one, which is the distinction the non-optional `SpxPartition.Vni` cannot
// carry on its own.
//
// The method trusts that the request has already been Validated and that the
// handler has performed any cross-context checks Validate cannot see
// (org/tenant association, Site readiness, name uniqueness). It returns no
// error.
func (sxpcr *APISpectrumXPartitionCreateRequest) ToProto(sxp *cdbm.SpectrumXPartition) *corev1.SpxPartitionCreationRequest {
	sxpProto := sxp.ToProto()
	req := &corev1.SpxPartitionCreationRequest{
		Id:                   sxpProto.Id,
		Metadata:             sxpProto.Metadata,
		TenantOrganizationId: sxpProto.TenantOrganizationId,
	}
	if sxpcr.VNI != nil {
		vni := uint32(*sxpcr.VNI)
		req.Vni = &vni
	}
	return req
}

// APISpectrumXPartition is the data structure to capture API representation of a SpectrumX Partition
type APISpectrumXPartition struct {
	// ID is the unique UUID v4 identifier for the SpectrumX Partition
	ID string `json:"id"`
	// Name is the name of the SpectrumX Partition
	Name string `json:"name"`
	// Description is the description of the SpectrumX Partition
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the Tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// VNI is the VXLAN Network Identifier allocated for the SpectrumX Partition
	VNI *int `json:"vni"`
	// Labels is the labels of the SpectrumX Partition
	Labels map[string]string `json:"labels"`
	// Status is the status of the SpectrumX Partition
	Status cdbm.SpectrumXPartitionStatus `json:"status"`
	// StatusHistory is the status detail records for the SpectrumX Partition over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the SpectrumX Partition was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the SpectrumX Partition was last updated
	Updated time.Time `json:"updated"`
}

// FromDB populates this APISpectrumXPartition from its DB layer representation and the
// supplied status detail history. A nil DB model is a no-op.
func (asxp *APISpectrumXPartition) FromDB(dsxp *cdbm.SpectrumXPartition, dbsds []cdbm.StatusDetail) {
	if dsxp == nil {
		return
	}

	asxp.ID = dsxp.ID.String()
	asxp.Name = dsxp.Name
	asxp.Description = dsxp.Description
	asxp.SiteID = dsxp.SiteID.String()
	asxp.TenantID = dsxp.TenantID.String()
	asxp.VNI = dsxp.VNI
	asxp.Labels = dsxp.Labels
	asxp.Status = dsxp.Status
	asxp.Created = dsxp.Created
	asxp.Updated = dsxp.Updated

	asxp.Site = nil
	if dsxp.Site != nil {
		asxp.Site = NewAPISiteSummary(dsxp.Site)
	}

	asxp.Tenant = nil
	if dsxp.Tenant != nil {
		asxp.Tenant = NewAPITenantSummary(dsxp.Tenant)
	}

	asxp.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		asxp.StatusHistory = append(asxp.StatusHistory, NewAPIStatusDetail(dbsd))
	}
}

// APISpectrumXPartitionSummary is the data structure to capture API summary of a SpectrumXPartition
type APISpectrumXPartitionSummary struct {
	// ID of the SpectrumX Partition
	ID string `json:"id"`
	// Name of the SpectrumX Partition
	Name string `json:"name"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// VNI is the VXLAN Network Identifier allocated for the SpectrumX Partition
	VNI *int `json:"vni"`
	// Status is the status of the SpectrumX Partition
	Status cdbm.SpectrumXPartitionStatus `json:"status"`
}

// NewAPISpectrumXPartitionSummary accepts a DB layer SpectrumXPartition object and returns an API layer object
func NewAPISpectrumXPartitionSummary(dbsxp *cdbm.SpectrumXPartition) *APISpectrumXPartitionSummary {
	if dbsxp == nil {
		return nil
	}
	apisxps := APISpectrumXPartitionSummary{
		ID:     dbsxp.ID.String(),
		Name:   dbsxp.Name,
		SiteID: dbsxp.SiteID.String(),
		VNI:    dbsxp.VNI,
		Status: dbsxp.Status,
	}
	return &apisxps
}
