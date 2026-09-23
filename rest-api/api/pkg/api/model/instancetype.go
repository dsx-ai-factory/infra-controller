// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIInstanceTypeCreateRequest is the data structure to capture user request to create a new InstanceType
type APIInstanceTypeCreateRequest struct {
	// Name is the name of the InstanceType
	Name string `json:"name"`
	// Description is the description of the Instance Type
	Description *string `json:"description"`
	// SiteID is the ID of the site
	SiteID string `json:"siteId"`
	// Labels is the labels of the Instance Type
	Labels map[string]string `json:"labels"`
	// ControllerMachineType is the Site Controller assigned Machine type
	ControllerMachineType *string `json:"controllerMachineType"`
	// MachineCapabilities is the list of Machine Capabilities to match
	MachineCapabilities APIMachineCapabilities `json:"machineCapabilities"`
}

// Validate ensure the values passed in request are acceptable
func (itcr *APIInstanceTypeCreateRequest) Validate() error {
	return validation.ValidateStruct(itcr,
		validation.Field(&itcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&itcr.ControllerMachineType,
			validation.When(itcr.ControllerMachineType != nil, validation.Length(2, 0).Error("not a valid value"))),
		validation.Field(&itcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&itcr.Labels, validation.By(util.ValidateLabels)),
		validation.Field(&itcr.MachineCapabilities),
	)
}

// ToProto builds the workflow request that asks a Site to create this
// InstanceType. `it` is the just-persisted DB record with its
// `Capabilities` slice pre-loaded by the handler; its `ToProto()` is
// the source of the canonical wire fields (Id/Metadata/Attributes
// including the desired-capabilities filter).
//
// The method trusts that the request has already been Validated. The
// per-capability wire rules (device type / InactiveDevices / numeric
// bounds) are enforced by `Validate` so this method stays a pure
// mapper.
func (itcr *APIInstanceTypeCreateRequest) ToProto(it *cdbm.InstanceType) *corev1.CreateInstanceTypeRequest {
	itProto := it.ToProto()
	return &corev1.CreateInstanceTypeRequest{
		Id:                     &itProto.Id,
		Metadata:               itProto.Metadata,
		InstanceTypeAttributes: itProto.Attributes,
	}
}

// APIInstanceTypeUpdateRequest is the data structure to capture user request to update an Instance Type
type APIInstanceTypeUpdateRequest struct {
	// Name is the name of the Instance Type
	Name *string `json:"name"`
	// Description is the description of the Instance Type
	Description *string `json:"description"`
	// Labels is the labels of the Instance Type
	Labels map[string]string `json:"labels"`
	// MachineCapabilities is the list of Machine Capabilities to match
	MachineCapabilities APIMachineCapabilities `json:"machineCapabilities"`
}

// Validate ensure the values passed in request are acceptable
func (itur *APIInstanceTypeUpdateRequest) Validate() error {
	return validation.ValidateStruct(itur,
		validation.Field(&itur.Name,
			// length validation rule accepts empty string as valid, hence, required is needed
			validation.When(itur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(itur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(itur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&itur.Labels, validation.By(util.ValidateLabels)),
		validation.Field(&itur.MachineCapabilities),
	)
}

// ToProto builds the workflow request that pushes this Update's
// merged-into-DB state to a Site. `it` is the post-merge DB record
// with its `Capabilities` slice pre-loaded by the handler; its
// `ToProto()` is the source of the canonical wire fields
// (Id/Metadata/Attributes including desired-capabilities filter) so
// unchanged fields stay populated.
//
// The method trusts that the request has already been Validated.
func (itur *APIInstanceTypeUpdateRequest) ToProto(it *cdbm.InstanceType) *corev1.UpdateInstanceTypeRequest {
	itProto := it.ToProto()
	return &corev1.UpdateInstanceTypeRequest{
		Id:                     itProto.Id,
		Metadata:               itProto.Metadata,
		InstanceTypeAttributes: itProto.Attributes,
	}
}

// APIInstanceType is the data structure to capture API representation of an Instance Type
type APIInstanceType struct {
	// ID is the unique UUID v4 identifier for the Instance Type
	ID string `json:"id"`
	// Name is the name of the Instance Type
	Name string `json:"name"`
	// Description is the description of the Instance Type
	Description *string `json:"description"`
	// ControllerMachineType is the Machine type assigned by Site Controller
	ControllerMachineType *string `json:"controllerMachineType"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider that owns the Instance Type
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// SiteID is the ID of the Site that owns the Instance Type
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// Labels is the labels of the Instance Type
	Labels APILabels `json:"labels"`
	// MachineCapabilities is the list of capabilities that are supported by the Machine's of this Instance Type
	MachineCapabilities []APIMachineCapability `json:"machineCapabilities"`
	// MachineInstanceTypes is the list of machines that are associated to this Instance Type
	MachineInstanceTypes []APIMachineInstanceType `json:"machineInstanceTypes,omitempty"`
	// AllocationStats is the stats of allocation that are associated to this Instance Type
	AllocationStats *APIInstanceTypeAllocationStats `json:"allocationStats,omitempty"`
	// Deprecations is the list of deprecation messages denoting fields which are being deprecated
	Deprecations []APIDeprecation `json:"deprecations,omitempty"`
	// Status is the status of the Instance Type
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the Instance Type
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created is the date and time the entity was created
	Created time.Time `json:"created"`
	// Updated is the date and time the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIInstanceType accepts a DB layer Instance Type object returns an API layer object
func NewAPIInstanceType(dbit *cdbm.InstanceType, dbsds []cdbm.StatusDetail, mcs []cdbm.MachineCapability, mit []cdbm.MachineInstanceType, aas *APIInstanceTypeAllocationStats) *APIInstanceType {
	if dbit == nil {
		return nil
	}

	apiit := &APIInstanceType{
		ID:                       dbit.ID.String(),
		Name:                     dbit.Name,
		Description:              dbit.Description,
		ControllerMachineType:    dbit.ControllerMachineType,
		InfrastructureProviderID: dbit.InfrastructureProviderID.String(),
		SiteID:                   dbit.SiteID.String(),
		Labels:                   APILabels(dbit.Labels),
		Status:                   dbit.Status,
		Created:                  dbit.Created,
		Updated:                  dbit.Updated,
	}

	apiit.AllocationStats = aas

	if dbit.InfrastructureProvider != nil {
		apiit.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbit.InfrastructureProvider)
	}

	if dbit.Site != nil {
		apiit.Site = NewAPISiteSummary(dbit.Site)
	}

	apiit.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiit.StatusHistory = append(apiit.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	apiit.MachineCapabilities = []APIMachineCapability{}
	for _, mc := range mcs {
		cmc := mc
		apiit.MachineCapabilities = append(apiit.MachineCapabilities, *NewAPIMachineCapability(&cmc))
	}

	apiit.MachineInstanceTypes = []APIMachineInstanceType{}
	for _, mi := range mit {
		cmi := mi
		apiit.MachineInstanceTypes = append(apiit.MachineInstanceTypes, *NewAPIMachineInstanceType(&cmi))
	}

	return apiit
}

// APIInstanceTypeSummary is the data structure to capture summary of an Instance Type
type APIInstanceTypeSummary struct {
	// ID of the Instance Type
	ID string `json:"id"`
	// Name of the InstanceType, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider that owns the Instance Type
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// SiteID is the ID of the Site that owns the Instance Type
	SiteID string `json:"siteId"`
	// Status is the status of the Instance Type
	Status string `json:"status"`
}

// NewAPIInstanceTypeSummary accepts a DB layer Instance object returns an API layer summary object
func NewAPIInstanceTypeSummary(dbist *cdbm.InstanceType) *APIInstanceTypeSummary {
	inst := APIInstanceTypeSummary{
		ID:                       dbist.ID.String(),
		Name:                     dbist.Name,
		InfrastructureProviderID: dbist.InfrastructureProviderID.String(),
		SiteID:                   dbist.SiteID.String(),
		Status:                   dbist.Status,
	}

	return &inst
}

// APIAllocationStats is the data structure to capture API representation of an InstanceType allocation stats
type APIInstanceTypeAllocationStats struct {
	// Assigned is the total number of Machines assigned to this Instance Type
	Assigned int `json:"assigned"`
	// Total is the total number of Machines allocated to different Tenants for this Instance Type
	Total int `json:"total"`
	// Used is the total number of allocated Machines of this Instance Type currently being used by Tenants
	Used int `json:"used"`
	// Unused is the total number of allocated Machines of this Instance Type that is currently not being used by Tenants
	Unused int `json:"unused"`
	// UnusedUsable is the total number of allocated Machines of this Instance Type that is currently not in use
	// but in Ready state, therefore can be provisioned by Tenant
	UnusedUsable int `json:"unusedUsable"`
	// MaxAllocatable is the maximum number of Machines of this Instance Type that can be allocated to a Tenant
	MaxAllocatable *int `json:"maxAllocatable,omitempty"`
}
