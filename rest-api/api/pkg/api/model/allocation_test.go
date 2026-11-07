// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPIAllocationCreateRequest_Validate(t *testing.T) {
	testAllocationConstraints := []APIAllocationConstraintCreateRequest{
		{
			ResourceType:    cdbm.AllocationResourceTypeIPBlock,
			ResourceTypeID:  uuid.New().String(),
			ConstraintType:  cdbm.AllocationConstraintTypeOnDemand,
			ConstraintValue: 5,
		},
	}
	testAllocationConstraintsMultiple := []APIAllocationConstraintCreateRequest{
		{
			ResourceType:    cdbm.AllocationResourceTypeIPBlock,
			ResourceTypeID:  uuid.New().String(),
			ConstraintType:  cdbm.AllocationConstraintTypeOnDemand,
			ConstraintValue: 5,
		},
		{
			ResourceType:    cdbm.AllocationResourceTypeIPBlock,
			ResourceTypeID:  uuid.New().String(),
			ConstraintType:  cdbm.AllocationConstraintTypeOnDemand,
			ConstraintValue: 5,
		},
	}
	testAllocationConstraintsInvalid := []APIAllocationConstraintCreateRequest{
		{
			ResourceType:    "badvalue",
			ResourceTypeID:  uuid.New().String(),
			ConstraintType:  cdbm.AllocationConstraintTypeOnDemand,
			ConstraintValue: 5,
		},
	}
	tests := []struct {
		desc      string
		obj       APIAllocationCreateRequest
		expectErr bool
	}{
		{
			desc:      "error when name is not specified",
			obj:       APIAllocationCreateRequest{Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: true,
		},
		{
			desc:      "errors when name is invalid",
			obj:       APIAllocationCreateRequest{Name: "a", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: true,
		},
		{
			desc:      "ok when description is empty",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr(""), TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: false,
		},
		{
			desc:      "error when tenantID is not specified",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: true,
		},
		{
			desc:      "error when tenantID is invalid",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: "some", SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: true,
		},
		{
			desc:      "error when siteID is not specified",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: true,
		},
		{
			desc:      "error when siteID is invalid",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: "some", AllocationConstraints: testAllocationConstraints},
			expectErr: true,
		},
		{
			desc:      "error when allocation constraints are not specified",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: uuid.New().String()},
			expectErr: true,
		},
		{
			desc:      "error when allocation constraints are more than 1",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraintsMultiple},
			expectErr: true,
		},
		{
			desc:      "error when allocation constraints are 1 but invalid",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraintsInvalid},
			expectErr: true,
		},
		{
			desc:      "ok with valid values, with description",
			obj:       APIAllocationCreateRequest{Name: "abcd", Description: cutil.GetPtr("some"), TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: false,
		},
		{
			desc:      "ok with valid values, without description",
			obj:       APIAllocationCreateRequest{Name: "abcd", TenantID: uuid.New().String(), SiteID: uuid.New().String(), AllocationConstraints: testAllocationConstraints},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
		})
	}
}

func TestAPIAllocationUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIAllocationUpdateRequest
		expectErr bool
	}{
		{
			desc:      "ok when name is not specified",
			obj:       APIAllocationUpdateRequest{Description: cutil.GetPtr("some")},
			expectErr: false,
		},
		{
			desc:      "errors when name is invalid",
			obj:       APIAllocationUpdateRequest{Name: cutil.GetPtr("a"), Description: cutil.GetPtr("some")},
			expectErr: true,
		},
		{
			desc:      "errors when name is invalid with empty",
			obj:       APIAllocationUpdateRequest{Name: cutil.GetPtr(""), Description: cutil.GetPtr("some")},
			expectErr: true,
		},
		{
			desc:      "ok when description is empty",
			obj:       APIAllocationUpdateRequest{Name: cutil.GetPtr("abcd"), Description: cutil.GetPtr("")},
			expectErr: false,
		},
		{
			desc:      "ok when name and description are specified",
			obj:       APIAllocationUpdateRequest{Name: cutil.GetPtr("abcd"), Description: cutil.GetPtr("some")},
			expectErr: false,
		},
		{
			desc:      "ok when description is not specified",
			obj:       APIAllocationUpdateRequest{Name: cutil.GetPtr("abcd")},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
		})
	}
}

func TestAPIAllocation_New(t *testing.T) {
	dbac1 := cdbm.AllocationConstraint{
		ID:                uuid.New(),
		AllocationID:      uuid.New(),
		ResourceType:      cdbm.AllocationResourceTypeInstanceType,
		ResourceTypeID:    uuid.New(),
		ConstraintType:    cdbm.AllocationConstraintTypeReserved,
		ConstraintValue:   5,
		DerivedResourceID: nil,
		Created:           time.Now(),
		Updated:           time.Now(),
		CreatedBy:         uuid.New(),
	}
	dbac2 := cdbm.AllocationConstraint{
		ID:                uuid.New(),
		AllocationID:      uuid.New(),
		ResourceType:      cdbm.AllocationResourceTypeIPBlock,
		ResourceTypeID:    uuid.New(),
		ConstraintType:    cdbm.AllocationConstraintTypeReserved,
		ConstraintValue:   5,
		DerivedResourceID: nil,
		Created:           time.Now(),
		Updated:           time.Now(),
		CreatedBy:         uuid.New(),
	}
	dba := &cdbm.Allocation{
		ID:                       uuid.New(),
		Name:                     "test",
		Description:              nil,
		InfrastructureProviderID: uuid.New(),
		TenantID:                 uuid.New(),
		SiteID:                   uuid.New(),
		Status:                   cdbm.AllocationStatusPending,
		Created:                  time.Now(),
		Updated:                  time.Now(),
		CreatedBy:                uuid.New(),
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dba.ID.String(),
			Status:   cdbm.TenantAccountStatusPending,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	dbacsInstanceTypeMap := map[uuid.UUID]*cdbm.InstanceType{}
	dbacsInstanceTypeMap[dbac1.ID] = &cdbm.InstanceType{
		ID:                       dbac1.ResourceTypeID,
		SiteID:                   &dba.SiteID,
		InfrastructureProviderID: dba.InfrastructureProviderID,
		Name:                     "Test",
		Description:              cutil.GetPtr("test"),
	}

	apia := NewAPIAllocation(dba, dbsds, []cdbm.AllocationConstraint{dbac1, dbac2}, dbacsInstanceTypeMap, nil)
	assert.Equal(t, dba.ID.String(), apia.ID)
	assert.Equal(t, dba.Name, apia.Name)
	assert.Equal(t, dba.TenantID.String(), apia.TenantID)
	assert.Equal(t, dba.SiteID.String(), apia.SiteID)
	assert.Equal(t, 2, len(apia.AllocationConstraints))
	assert.Equal(t, 1, len(apia.StatusHistory))
}
