// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var (
	// ErrValidationNVLinkLogicalPartitionAssociation is the error when no associations are specified in the security group
	ErrValidationNVLinkLogicalPartitionAssociation = errors.New("at least one security group association is required")
)

// APINVLinkLogicalPartitionCreateRequest is the data structure to capture instance request to create a new NVLinkLogicalPartition
type APINVLinkLogicalPartitionCreateRequest struct {
	// Name is the name of the NVLinkLogicalPartition
	Name string `json:"name"`
	// Description is the description of the NVLinkLogicalPartition
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
}

// Validate ensure the values passed in request are acceptable
func (anlpcr *APINVLinkLogicalPartitionCreateRequest) Validate() error {
	err := validation.ValidateStruct(anlpcr,
		validation.Field(&anlpcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&anlpcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}
	return nil
}

// ToProto builds the workflow request that asks a Site to create the
// NVLink Logical Partition represented by this API request. `nvllp` is
// the just-persisted DB record; its `ToProto()` is the source of the
// canonical wire fields (id, metadata, tenant organization id).
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see.
func (anlpcr *APINVLinkLogicalPartitionCreateRequest) ToProto(nvllp *cdbm.NVLinkLogicalPartition) *corev1.NVLinkLogicalPartitionCreationRequest {
	entityProto := nvllp.ToProto()
	return &corev1.NVLinkLogicalPartitionCreationRequest{
		Id:     entityProto.Id,
		Config: entityProto.Config,
	}
}

// APINVLinkLogicalPartitionUpdateRequest is the data structure to capture user request to update a NVLinkLogicalPartition
type APINVLinkLogicalPartitionUpdateRequest struct {
	// Name is the name of the NVLinkLogicalPartition
	Name *string `json:"name"`
	// Description is the description of the NVLinkLogicalPartition
	Description *string `json:"description"`
}

// Validate ensure the values passed in request are acceptable
func (anlpur *APINVLinkLogicalPartitionUpdateRequest) Validate() error {
	return validation.ValidateStruct(anlpur,
		validation.Field(&anlpur.Name,
			validation.When(anlpur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(anlpur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(anlpur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
	)
}

// ToProto builds the workflow request that asks a Site to update the
// NVLink Logical Partition represented by this API request. `nvllp` is
// the post-update DB record; its `ToProto()` is the source of the
// canonical wire fields. NICo requires Metadata.Name on every update,
// which the entity's `ToProto()` always populates from the entity's
// current Name (preserving the existing handler comment's intent even
// when the client sends only `description`).
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see.
func (anlpur *APINVLinkLogicalPartitionUpdateRequest) ToProto(nvllp *cdbm.NVLinkLogicalPartition) *corev1.NVLinkLogicalPartitionUpdateRequest {
	entityProto := nvllp.ToProto()
	return &corev1.NVLinkLogicalPartitionUpdateRequest{
		Id:     entityProto.Id,
		Config: entityProto.Config,
	}
}

// APINVLinkLogicalPartition is the data structure to capture API representation of a NVLinkLogicalPartition
type APINVLinkLogicalPartition struct {
	// ID is the unique UUID v4 identifier for the NVLinkLogicalPartition
	ID string `json:"id"`
	// Name is the name of the NVLinkLogicalPartition
	Name string `json:"name"`
	// Description is the description of the NVLinkLogicalPartition
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Vpcs is the list of VPCs associated with the NVLinkLogicalPartition
	Vpcs []APIVpcSummary `json:"vpcs,omitempty"`
	// NVLinkInterfaces is the list of NVLinkInterfaces associated with the NVLinkLogicalPartition
	NVLinkInterfaces []APINVLinkInterfaceSummary `json:"nvLinkInterfaces,omitempty"`
	// NVLinkLogicalPartitionStats holds GPU and instance counts for a NVLinkLogicalPartition
	NVLinkLogicalPartitionStats *APINVLinkLogicalPartitionStats `json:"nvLinkLogicalPartitionStats"`
	// Status is the status o the NVLinkLogicalPartition
	Status cdbm.NVLinkLogicalPartitionStatus `json:"status"`
	// StatusHistory is the status detail records for the NVLinkLogicalPartition over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the NVLinkLogicalPartition was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the NVLinkLogicalPartition was last updated
	Updated time.Time `json:"updated"`
}

// NewAPINVLinkLogicalPartition accepts a DB layer NVLinkLogicalPartition object and returns an API object
func NewAPINVLinkLogicalPartition(dbnlp *cdbm.NVLinkLogicalPartition, dbvpcs []cdbm.Vpc, dbnvlifcs []cdbm.NVLinkInterface, dbsds []cdbm.StatusDetail) *APINVLinkLogicalPartition {
	apinlplp := &APINVLinkLogicalPartition{
		ID:          dbnlp.ID.String(),
		Name:        dbnlp.Name,
		Description: dbnlp.Description,
		SiteID:      dbnlp.SiteID.String(),
		TenantID:    dbnlp.TenantID.String(),
		Status:      dbnlp.Status,
		Created:     dbnlp.Created,
		Updated:     dbnlp.Updated,
	}

	if dbnlp.Site != nil {
		apinlplp.Site = NewAPISiteSummary(dbnlp.Site)
	}

	if dbnlp.Tenant != nil {
		apinlplp.Tenant = NewAPITenantSummary(dbnlp.Tenant)
	}

	apinlplp.Vpcs = []APIVpcSummary{}
	for _, dbvpc := range dbvpcs {
		curnvpc := dbvpc
		apinlplp.Vpcs = append(apinlplp.Vpcs, *NewAPIVpcSummary(&curnvpc))
	}

	apinlplp.NVLinkInterfaces = []APINVLinkInterfaceSummary{}
	for _, dbnvlifc := range dbnvlifcs {
		curnvlifc := dbnvlifc
		apinlplp.NVLinkInterfaces = append(apinlplp.NVLinkInterfaces, *NewAPINVLinkInterfaceSummary(&curnvlifc))
	}

	apinlplp.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apinlplp.StatusHistory = append(apinlplp.StatusHistory, NewAPIStatusDetail(dbsd))
	}
	return apinlplp
}

// APINVLinkLogicalPartitionSummary is the data structure to capture API summary of a NVLinkLogicalPartition
type APINVLinkLogicalPartitionSummary struct {
	// ID of the NVLinkLogicalPartition
	ID string `json:"id"`
	// Name of the NVLinkLogicalPartition
	Name string `json:"name"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Status is the status of the NVLinkLogicalPartition
	Status cdbm.NVLinkLogicalPartitionStatus `json:"status"`
}

// NewAPINVLinkLogicalPartitionSummary accepts a DB layer NVLinkLogicalPartition object returns an API layer object
func NewAPINVLinkLogicalPartitionSummary(dbnlp *cdbm.NVLinkLogicalPartition) *APINVLinkLogicalPartitionSummary {
	apinlplps := APINVLinkLogicalPartitionSummary{
		ID:     dbnlp.ID.String(),
		Name:   dbnlp.Name,
		SiteID: dbnlp.SiteID.String(),
		Status: dbnlp.Status,
	}
	return &apinlplps
}

type APINVLinkLogicalPartitionStats struct {
	TotalGpus              int `json:"totalGpus"`
	TotalDistinctInstances int `json:"totalDistinctInstances"`
}

// NewAPINVLinkLogicalPartitionStats creates and returns a new APINVLinkLogicalPartitionStats object
func NewAPINVLinkLogicalPartitionStats() *APINVLinkLogicalPartitionStats {
	return &APINVLinkLogicalPartitionStats{
		TotalGpus:              0,
		TotalDistinctInstances: 0,
	}
}
