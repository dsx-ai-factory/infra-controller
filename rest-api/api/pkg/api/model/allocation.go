// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	"github.com/google/uuid"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"

	"github.com/dsx-ai-factory/infra-controller/rest-api/api/pkg/api/model/util"
)

var (
	// ErrOneAllocationConstraintIsRequired is an error when one allocation constraint is not found in allocation
	ErrOneAllocationConstraintIsRequired = errors.New("at least one (and at most one) Allocation Constraint must be specified")
)

// APIAllocationCreateRequest captures user request to create a new Allocation
type APIAllocationCreateRequest struct {
	// Name is the name of the Allocation
	Name string `json:"name"`
	// Description is the description of the Allocation
	Description *string `json:"description"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// AllocationConstraints is a list of Allocation Constraint objects
	AllocationConstraints []APIAllocationConstraintCreateRequest `json:"allocationConstraints"`
}

// Validate ensure the values passed in request are acceptable
func (acr APIAllocationCreateRequest) Validate() error {
	err := validation.ValidateStruct(&acr,
		validation.Field(&acr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&acr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&acr.TenantID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}
	// validate the AllocationConstraints
	if len(acr.AllocationConstraints) != 1 {
		return validation.Errors{
			"allocationConstraints": ErrOneAllocationConstraintIsRequired,
		}
	}
	err = acr.AllocationConstraints[0].Validate()
	return err
}

// APIAllocationUpdateRequest is the data structure to capture user request to update an Allocation
type APIAllocationUpdateRequest struct {
	// Name is the name of the Allocation
	Name *string `json:"name"`
	// Description is the description of the Allocation
	Description *string `json:"description"`
}

// Validate ensure the values passed in request are acceptable
func (aur APIAllocationUpdateRequest) Validate() error {
	return validation.ValidateStruct(&aur,
		validation.Field(&aur.Name,
			// length validation rule accepts empty string as valid, hence, required is needed
			validation.When(aur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(aur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(aur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
	)
}

// APIAllocation is api representation of the Allocation
type APIAllocation struct {
	// ID is the ID of the allocation
	ID string `json:"id"`
	// Name is the name of the Allocation
	Name string `json:"name"`
	// Description is the description of the Allocation
	Description *string `json:"description"`
	// InfrastructureProviderID is the ID of the Infrastructure Provider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the Infrastructure Provider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the Tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// Status is the status of the Allocation
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the Allocation
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
	// AllocationConstraints is a list of Allocation Constraints for the Allocation
	AllocationConstraints []APIAllocationConstraint `json:"allocationConstraints"`
}

// NewAPIAllocation coverts db layer objects into API objects
func NewAPIAllocation(dba *cdbm.Allocation, dbsds []cdbm.StatusDetail, acs []cdbm.AllocationConstraint, dbacsInstaceTypeMap map[uuid.UUID]*cdbm.InstanceType, dbacsIPBlockMap map[uuid.UUID]*cdbm.IPBlock) *APIAllocation {
	apiAllocation := APIAllocation{
		ID:                       dba.ID.String(),
		Name:                     dba.Name,
		Description:              dba.Description,
		InfrastructureProviderID: dba.InfrastructureProviderID.String(),
		TenantID:                 dba.TenantID.String(),
		SiteID:                   dba.SiteID.String(),
		Status:                   dba.Status,
		Created:                  dba.Created,
		Updated:                  dba.Updated,
	}

	apiAllocation.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiAllocation.StatusHistory = append(apiAllocation.StatusHistory, NewAPIStatusDetail(dbsd))
	}
	apiAllocation.AllocationConstraints = []APIAllocationConstraint{}
	for _, ac := range acs {
		tmpAC := ac
		if ac.ResourceType == cdbm.AllocationResourceTypeInstanceType {
			dbinstanceType, ok := dbacsInstaceTypeMap[ac.ID]
			if ok {
				apiAllocation.AllocationConstraints = append(apiAllocation.AllocationConstraints, *NewAPIAllocationConstraint(&tmpAC, dbinstanceType, nil))
			} else {
				apiAllocation.AllocationConstraints = append(apiAllocation.AllocationConstraints, *NewAPIAllocationConstraint(&tmpAC, nil, nil))
			}
		} else if ac.ResourceType == cdbm.AllocationResourceTypeIPBlock {
			dbipb, ok := dbacsIPBlockMap[ac.ID]
			if ok {
				apiAllocation.AllocationConstraints = append(apiAllocation.AllocationConstraints, *NewAPIAllocationConstraint(&tmpAC, nil, dbipb))
			} else {
				apiAllocation.AllocationConstraints = append(apiAllocation.AllocationConstraints, *NewAPIAllocationConstraint(&tmpAC, nil, nil))
			}
		}
	}

	if dba.InfrastructureProvider != nil {
		apiAllocation.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dba.InfrastructureProvider)
	}

	if dba.Tenant != nil {
		apiAllocation.Tenant = NewAPITenantSummary(dba.Tenant)
	}

	if dba.Site != nil {
		apiAllocation.Site = NewAPISiteSummary(dba.Site)
	}

	return &apiAllocation
}
