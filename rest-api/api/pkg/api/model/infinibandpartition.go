// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

var (
	// ErrValidationInfiniBandPartitionAssociation is the error when no associations are specified in the security group
	ErrValidationInfiniBandPartitionAssociation = errors.New("at least one security group association is required")
)

// APIInfiniBandPartitionCreateRequest is the data structure to capture instance request to create a new InfiniBandPartition
type APIInfiniBandPartitionCreateRequest struct {
	// Name is the name of the InfiniBand Partition
	Name string `json:"name"`
	// Description is the description of the InfiniBand Partition
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Labels is the labels of the InfiniBand Partition
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (ibpcr *APIInfiniBandPartitionCreateRequest) Validate() error {
	err := validation.ValidateStruct(ibpcr,
		validation.Field(&ibpcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&ibpcr.Description,
			validation.When(ibpcr.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&ibpcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)

	if err != nil {
		return err
	}

	return util.ValidateLabels(ibpcr.Labels)
}

// ToProto builds the workflow request that asks a Site to create this
// InfiniBand Partition for this API request. `ibp` is the
// just-persisted DB record; its `ToProto()` is the source of the
// canonical wire fields (Id / Config.Name / Config.TenantOrganizationId
// / Metadata) which the create request reuses verbatim.
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see (org/tenant association, Site readiness, name
// uniqueness). It returns no error.
func (ibpcr *APIInfiniBandPartitionCreateRequest) ToProto(ibp *cdbm.InfiniBandPartition) *corev1.IBPartitionCreationRequest {
	ibpProto := ibp.ToProto()
	return &corev1.IBPartitionCreationRequest{
		Id:       ibpProto.Id,
		Config:   ibpProto.Config,
		Metadata: ibpProto.Metadata,
	}
}

// APIInfiniBandPartitionUpdateRequest is the data structure to capture user request to update a InfiniBandPartition
type APIInfiniBandPartitionUpdateRequest struct {
	// Name is the name of the InfiniBand Partition
	Name *string `json:"name"`
	// Description is the description of the InfiniBand Partition
	Description *string `json:"description"`
	// Labels is the labels of the InfiniBand Partition
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (ibpur *APIInfiniBandPartitionUpdateRequest) Validate() error {
	err := validation.ValidateStruct(ibpur,
		validation.Field(&ibpur.Name,
			validation.When(ibpur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(ibpur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(ibpur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&ibpur.Description,
			validation.When(ibpur.Description != nil, validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
	)

	if err != nil {
		return err
	}

	return util.ValidateLabels(ibpur.Labels)
}

// ToProto builds the workflow request that pushes this Update's
// merged-into-DB state to a Site. The persisted `ibp` is the source
// of the wire fields because the handler has already merged the
// request's (sparse) update fields into the entity by the time this
// is called; sending the post-merge state matches the pre-existing
// handler behaviour and keeps unchanged fields populated.
//
// The method trusts that the request has already been Validated. It
// returns no error.
func (ibpur *APIInfiniBandPartitionUpdateRequest) ToProto(ibp *cdbm.InfiniBandPartition) *corev1.IBPartitionUpdateRequest {
	ibpProto := ibp.ToProto()
	return &corev1.IBPartitionUpdateRequest{
		Id:       ibpProto.Id,
		Config:   ibpProto.Config,
		Metadata: ibpProto.Metadata,
	}
}

// APIInfiniBandPartition is the data structure to capture API representation of a InfiniBand Partition
type APIInfiniBandPartition struct {
	// ID is the unique UUID v4 identifier for the InfiniBand Partition
	ID string `json:"id"`
	// Name is the name of the InfiniBand Partition
	Name string `json:"name"`
	// Description is the description of the InfiniBand Partition
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Controller IB Partition ID is the ID of the Site Controller IB partition
	ControllerIBPartitionID *string `json:"controllerIBPartitionId"`
	// Partition Key is the key of IB partition
	PartitionKey *string `json:"partitionKey"`
	// Partition Name is the name of IB partition
	PartitionName *string `json:"partitionName"`
	// Service Level is the service level of IB partition
	ServiceLevel *int `json:"serviceLevel"`
	// Rate Limit is the rate limit of IB partition
	RateLimit *float32 `json:"rateLimit"`
	// Mtu of the IB partition
	Mtu *int `json:"mtu"`
	// EnableSharp indicates if sharp enable on the IB partition or not
	EnableSharp *bool `json:"enableSharp"`
	// Labels is the labels of the InfiniBand Partition
	Labels APILabels `json:"labels"`
	// Status is the status o the InfiniBand Partition
	Status cdbm.InfiniBandPartitionStatus `json:"status"`
	// StatusHistory is the status detail records for the InfiniBand Partition over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the InfiniBand Partition was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the InfiniBand Partition was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIInfiniBandPartition accepts a DB layer InfiniBandPartition object and returns an API object
func NewAPIInfiniBandPartition(dibp *cdbm.InfiniBandPartition, dbsds []cdbm.StatusDetail) *APIInfiniBandPartition {
	apiibp := &APIInfiniBandPartition{
		ID:            dibp.ID.String(),
		Name:          dibp.Name,
		Description:   dibp.Description,
		SiteID:        dibp.SiteID.String(),
		TenantID:      dibp.TenantID.String(),
		PartitionKey:  dibp.PartitionKey,
		PartitionName: dibp.PartitionName,
		ServiceLevel:  dibp.ServiceLevel,
		RateLimit:     dibp.RateLimit,
		Mtu:           dibp.Mtu,
		Labels:        APILabels(dibp.Labels),
		EnableSharp:   dibp.EnableSharp,
		Status:        dibp.Status,
		Created:       dibp.Created,
		Updated:       dibp.Updated,
	}

	if dibp.ControllerIBPartitionID != nil {
		apiibp.ControllerIBPartitionID = util.GetUUIDPtrToStrPtr(dibp.ControllerIBPartitionID)
	}

	if dibp.Site != nil {
		apiibp.Site = NewAPISiteSummary(dibp.Site)
	}

	if dibp.Tenant != nil {
		apiibp.Tenant = NewAPITenantSummary(dibp.Tenant)
	}

	apiibp.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiibp.StatusHistory = append(apiibp.StatusHistory, NewAPIStatusDetail(dbsd))
	}
	return apiibp
}

// APIInfiniBandPartitionSummary is the data structure to capture API summary of a InfiniBandPartition
type APIInfiniBandPartitionSummary struct {
	// ID of the InfiniBand Partition
	ID string `json:"id"`
	// Name of the InfiniBand Partition
	Name string `json:"name"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Controller IB Partition is the ID of the Site Controller Partition corresponding to the InfiniBand Partition
	ControllerIBPartitionID *string `json:"controllerIBPartitionId"`
	// Status is the status of the InfiniBand Partition
	Status cdbm.InfiniBandPartitionStatus `json:"status"`
}

// NewAPIInfiniBandPartitionSummary accepts a DB layer InfiniBandPartition object returns an API layer object
func NewAPIInfiniBandPartitionSummary(dbibp *cdbm.InfiniBandPartition) *APIInfiniBandPartitionSummary {
	apiibps := APIInfiniBandPartitionSummary{
		ID:     dbibp.ID.String(),
		Name:   dbibp.Name,
		SiteID: dbibp.SiteID.String(),
		Status: dbibp.Status,
	}
	if dbibp.ControllerIBPartitionID != nil {
		apiibps.ControllerIBPartitionID = util.GetUUIDPtrToStrPtr(dbibp.ControllerIBPartitionID)
	}
	return &apiibps
}
