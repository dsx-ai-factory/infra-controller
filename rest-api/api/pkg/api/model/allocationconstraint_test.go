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

func TestAPIAllocationConstraintCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIAllocationConstraintCreateRequest
		expectErr bool
	}{
		{
			desc:      "errors when resource type is empty",
			obj:       APIAllocationConstraintCreateRequest{ResourceTypeID: uuid.New().String(), ConstraintType: cdbm.AllocationConstraintTypeOnDemand, ConstraintValue: 5},
			expectErr: true,
		},
		{
			desc:      "errors when resource type is invalid",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: "some", ResourceTypeID: uuid.New().String(), ConstraintType: cdbm.AllocationConstraintTypeOnDemand, ConstraintValue: 5},
			expectErr: true,
		},
		{
			desc:      "error when resourceTypeID is empty",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ConstraintType: cdbm.AllocationConstraintTypeOnDemand, ConstraintValue: 5},
			expectErr: true,
		},
		{
			desc:      "error when resourceTypeID is not valid uuid",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: "some", ConstraintType: cdbm.AllocationConstraintTypeOnDemand, ConstraintValue: 5},
			expectErr: true,
		},
		{
			desc:      "error when constraintType is empty",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: uuid.New().String(), ConstraintValue: 5},
			expectErr: true,
		},
		{
			desc:      "error when constraintType is invalid value",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: uuid.New().String(), ConstraintType: "some", ConstraintValue: 5},
			expectErr: true,
		},
		{
			desc:      "error when constraint value is not specified",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: uuid.New().String(), ConstraintType: cdbm.AllocationConstraintTypeOnDemand},
			expectErr: true,
		},
		{
			desc:      "ok with valid values",
			obj:       APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: uuid.New().String(), ConstraintType: cdbm.AllocationConstraintTypeOnDemand, ConstraintValue: 5},
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

func TestAPIAllocationConstraintUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIAllocationConstraintUpdateRequest
		expectErr bool
	}{
		{
			desc:      "error when constraint value is not specified",
			obj:       APIAllocationConstraintUpdateRequest{},
			expectErr: true,
		},
		{
			desc:      "ok with valid values",
			obj:       APIAllocationConstraintUpdateRequest{ConstraintValue: 5},
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

func TestAPIAllocationConstraint_New(t *testing.T) {
	dbac := &cdbm.AllocationConstraint{
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

	dbinstp := &cdbm.InstanceType{
		Name:                     "test-instancetype",
		DisplayName:              cutil.GetPtr("instance type"),
		InfrastructureProviderID: uuid.New(),
		SiteID:                   cutil.GetPtr(uuid.New()),
		Status:                   "Ready",
	}
	apiac := NewAPIAllocationConstraint(dbac, dbinstp, nil)
	assert.Equal(t, apiac.ID, dbac.ID.String())
	assert.Equal(t, apiac.AllocationID, dbac.AllocationID.String())
	assert.Equal(t, apiac.ResourceType, dbac.ResourceType)
	assert.Equal(t, apiac.ResourceTypeID, dbac.ResourceTypeID.String())
	assert.Equal(t, apiac.ConstraintType, dbac.ConstraintType)
	assert.Equal(t, apiac.ConstraintValue, dbac.ConstraintValue)
	assert.NotNil(t, apiac.InstanceType)
	assert.NotNil(t, apiac.InstanceType.Name, dbinstp.Name)
	assert.Nil(t, apiac.IPBlock)
}
