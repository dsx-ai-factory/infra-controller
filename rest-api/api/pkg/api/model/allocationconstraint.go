// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

const (
	// ValidationErrorAllocationConstraintResourceType indicates an invalid ResourceType field
	ValidationErrorAllocationConstraintResourceType = "Resource Type must be InstanceType or IPBlock"
	// ValidationErrorAllocationConstraintConstraintType indicates an invalid ConstraintType field
	ValidationErrorAllocationConstraintConstraintType = "Constraint Type should be Reserved, OnDemand or Preemptible"
)

var (
	// resourceTypeIDDeprecationTime is the time when the ResourceTypeID attribute will be no longer be available in the API
	resourceTypeIDDeprecationTime = time.Date(2026, time.September, 10, 0, 0, 0, 0, time.UTC)

	// resourceTypeIDDeprecations is a list of deprecated entities for the ResourceTypeID attribute
	allocationConstraintDeprecations = []DeprecatedEntity{
		{
			OldValue:     "ResourceTypeID",
			NewValue:     cutil.GetPtr("resourceTypeId"),
			Type:         DeprecationTypeAttribute,
			TakeActionBy: resourceTypeIDDeprecationTime,
		},
	}
)

// APIAllocationConstraintCreateRequest captures user request to create a new Allocation Constraint
type APIAllocationConstraintCreateRequest struct {
	// ResourceType is the type of the resource for the Allocation Constraint
	ResourceType string `json:"resourceType"`
	// ResourceTypeID is the ID of the Resource Type
	ResourceTypeID string `json:"resourceTypeId"`
	// ConstraintType is the type of the Allocation Constraint
	ConstraintType string `json:"constraintType"`
	// ConstraintValue is the value of the Allocation Constraint
	ConstraintValue int `json:"constraintValue"`
}

// Validate ensure the values passed in request are acceptable
func (accr APIAllocationConstraintCreateRequest) Validate() error {
	err := validation.ValidateStruct(&accr,
		validation.Field(&accr.ResourceType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(
				cdbm.AllocationResourceTypeInstanceType,
				cdbm.AllocationResourceTypeIPBlock,
			).Error(ValidationErrorAllocationConstraintResourceType)),
		validation.Field(&accr.ResourceTypeID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&accr.ConstraintType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(
				cdbm.AllocationConstraintTypeOnDemand,
				cdbm.AllocationConstraintTypePreemptible,
				cdbm.AllocationConstraintTypeReserved,
			).Error(ValidationErrorAllocationConstraintConstraintType)),
		validation.Field(&accr.ConstraintValue,
			validation.Required.Error(validationErrorValueRequired)),
	)

	// TODO: Validate the range of values for ConstraintValue
	// Depending on the constraint type - if there is such a validation required
	return err
}

// APIAllocationConstraintUpdateRequest captures user request to update an existing Allocation Constraint value
type APIAllocationConstraintUpdateRequest struct {
	// ConstraintValue is the value of the Allocation Constraint
	ConstraintValue int `json:"constraintValue"`
}

// Validate ensure the values passed in request are acceptable
func (accr APIAllocationConstraintUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&accr,
		validation.Field(&accr.ConstraintValue,
			validation.Required.Error(validationErrorValueRequired)),
	)
	return err
}

// APIAllocationConstraint is api representation of the Allocation Constraint
type APIAllocationConstraint struct {
	// ID is the unique UUID identified for the Allocation Constraint
	ID string `json:"id"`
	// AllocationID is the ID of the Allocation corresponding to the Allocation Constraint
	AllocationID string `json:"allocationId"`
	// ResourceType is the type of the Resource
	ResourceType string `json:"resourceType"`
	// ResourceTypeIDDeprecated is the deprecated improperly cased attribute
	ResourceTypeIDDeprecated *string `json:"ResourceTypeID,omitempty"`
	// ResourceTypeID is the ID of the resource corresponding to the Allocation Constraint
	ResourceTypeID string `json:"resourceTypeId"`
	// ConstraintType is the type of the Allocation Constraint
	ConstraintType string `json:"constraintType"`
	// ConstraintValue is the value of the Allocation Constraint
	ConstraintValue int `json:"constraintValue"`
	// DerivedResourceID is the ID of the derived resource
	DerivedResourceID *string `json:"derivedResourceId"`
	// InstanceType is the summary of the InstaceType
	InstanceType *APIInstanceTypeSummary `json:"instanceType,omitempty"`
	// IPBlock is the summary of the IPBlock
	IPBlock *APIIPBlockSummary `json:"ipBlock,omitempty"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
	// Deprecations is the list of deprecations for the Allocation Constraint
	Deprecations []APIDeprecation `json:"deprecations,omitempty"`
}

// NewAPIAllocationConstraint accepts a DB layer Allocation Constraint object and returns an API object
func NewAPIAllocationConstraint(cdbm *cdbm.AllocationConstraint, dbinstp *cdbm.InstanceType, dbipb *cdbm.IPBlock) *APIAllocationConstraint {
	apiac := &APIAllocationConstraint{
		ID:                cdbm.ID.String(),
		AllocationID:      cdbm.AllocationID.String(),
		ResourceType:      cdbm.ResourceType,
		ResourceTypeID:    cdbm.ResourceTypeID.String(),
		ConstraintType:    cdbm.ConstraintType,
		ConstraintValue:   cdbm.ConstraintValue,
		DerivedResourceID: util.GetUUIDPtrToStrPtr(cdbm.DerivedResourceID),
		Created:           cdbm.Created,
		Updated:           cdbm.Updated,
	}

	if dbinstp != nil {
		apiac.InstanceType = NewAPIInstanceTypeSummary(dbinstp)
	}

	if dbipb != nil {
		apiac.IPBlock = NewAPIIPBlockSummary(dbipb)
	}

	if time.Now().Before(resourceTypeIDDeprecationTime) {
		apiac.ResourceTypeIDDeprecated = cutil.GetPtr(cdbm.ResourceTypeID.String())
	}

	for _, deprecation := range allocationConstraintDeprecations {
		apiac.Deprecations = append(apiac.Deprecations, NewAPIDeprecation(deprecation))
	}

	return apiac
}
