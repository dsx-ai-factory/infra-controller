// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestAPIVpcPeeringCreateRequest_Validate(t *testing.T) {
	vpc1ID := uuid.New().String()
	vpc2ID := uuid.New().String()
	siteID := uuid.New().String()

	tests := []struct {
		desc      string
		obj       APIVpcPeeringCreateRequest
		expectErr bool
	}{
		{
			desc: "ok when all required fields are provided",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: vpc1ID,
				Vpc2ID: vpc2ID,
				SiteID: siteID,
			},
			expectErr: false,
		},
		{
			desc: "error when VPC1 is not a valid UUID",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: "invalid-uuid",
				Vpc2ID: vpc2ID,
				SiteID: siteID,
			},
			expectErr: true,
		},
		{
			desc: "error when VPC1 is missing",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: "",
				Vpc2ID: vpc2ID,
				SiteID: siteID,
			},
			expectErr: true,
		},
		{
			desc: "error when VPC2 is not a valid UUID",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: vpc1ID,
				Vpc2ID: "invalid-uuid",
				SiteID: siteID,
			},
			expectErr: true,
		},
		{
			desc: "error when VPC2 is missing",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: vpc1ID,
				Vpc2ID: "",
				SiteID: siteID,
			},
			expectErr: true,
		},
		{
			desc: "error when siteId is not a valid UUID",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: vpc1ID,
				Vpc2ID: vpc2ID,
				SiteID: "not-a-uuid",
			},
			expectErr: true,
		},
		{
			desc: "error when siteId is missing",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: vpc1ID,
				Vpc2ID: vpc2ID,
				SiteID: "",
			},
			expectErr: true,
		},
		{
			desc: "error when VPC1 and VPC2 are the same",
			obj: APIVpcPeeringCreateRequest{
				Vpc1ID: vpc1ID,
				Vpc2ID: vpc1ID,
				SiteID: siteID,
			},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIVpcPeeringCreateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	v1 := uuid.New()
	v2 := uuid.New()
	vp := &cdbm.VpcPeering{ID: id, Vpc1ID: v1, Vpc2ID: v2}

	req := APIVpcPeeringCreateRequest{
		Vpc1ID: v1.String(),
		Vpc2ID: v2.String(),
		SiteID: uuid.New().String(),
	}
	got := req.ToProto(vp)
	require.NotNil(t, got)
	require.NotNil(t, got.Id)
	assert.Equal(t, id.String(), got.Id.Value)
	require.NotNil(t, got.VpcId)
	assert.Equal(t, v1.String(), got.VpcId.Value)
	require.NotNil(t, got.PeerVpcId)
	assert.Equal(t, v2.String(), got.PeerVpcId.Value)
}

func TestNewAPIVpcPeering(t *testing.T) {
	now := time.Now()
	vpc1TenantID := uuid.New()
	vpc2TenantID := uuid.New()
	dbVpcPeering := cdbm.VpcPeering{
		ID:            uuid.New(),
		Vpc1ID:        uuid.New(),
		Vpc2ID:        uuid.New(),
		SiteID:        uuid.New(),
		IsMultiTenant: true,
		Status:        cdbm.VpcPeeringStatusReady,
		Created:       now,
		Updated:       now,
		Vpc1: &cdbm.Vpc{
			ID:       uuid.New(),
			TenantID: vpc1TenantID,
			Name:     "vpc-1",
			Status:   cdbm.VpcStatusReady,
		},
		Vpc2: &cdbm.Vpc{
			ID:       uuid.New(),
			TenantID: vpc2TenantID,
			Name:     "vpc-2",
			Status:   cdbm.VpcStatusReady,
		},
		TenantID: &vpc1TenantID,
		Tenant: &cdbm.Tenant{
			ID:             vpc1TenantID,
			Org:            "test-org",
			OrgDisplayName: util.GetPtr("Org Display name"),
		},
	}

	dbMappedPeeringTenantsMap := map[uuid.UUID]*cdbm.Tenant{
		vpc1TenantID: {
			ID:             vpc1TenantID,
			Org:            "test-org",
			OrgDisplayName: util.GetPtr("Org Display name"),
		},
		vpc2TenantID: {
			ID:             vpc2TenantID,
			Org:            "test-org",
			OrgDisplayName: util.GetPtr("Org Display name"),
		},
	}
	api := NewAPIVpcPeering(dbVpcPeering, dbMappedPeeringTenantsMap)

	assert.Equal(t, dbVpcPeering.ID.String(), api.ID)
	assert.Equal(t, dbVpcPeering.Vpc1ID.String(), api.Vpc1ID)
	assert.Equal(t, dbVpcPeering.Vpc2ID.String(), api.Vpc2ID)
	assert.Equal(t, dbVpcPeering.SiteID.String(), api.SiteID)
	assert.Equal(t, dbVpcPeering.IsMultiTenant, api.IsMultiTenant)
	assert.Equal(t, dbVpcPeering.Status, api.Status)
	assert.Equal(t, dbVpcPeering.Created, api.Created)
	assert.Equal(t, dbVpcPeering.Updated, api.Updated)
	require.NotNil(t, api.Vpc1)
	require.NotNil(t, api.Vpc1.Tenant)
	assert.Equal(t, vpc1TenantID.String(), api.Vpc1.Tenant.ID)
	require.NotNil(t, api.Vpc2)
	require.NotNil(t, api.Vpc2.Tenant)
	assert.Equal(t, vpc2TenantID.String(), api.Vpc2.Tenant.ID)
	require.NotNil(t, api.Tenant)
	require.NotNil(t, api.TenantID)
	assert.Equal(t, dbVpcPeering.TenantID.String(), *api.TenantID)
	assert.Equal(t, dbVpcPeering.Tenant.Org, api.Tenant.Org)
}

func TestNewAPIVpcPeeringVpcSummary(t *testing.T) {
	vpc1TenantID := uuid.New()
	dbVpc := &cdbm.Vpc{
		ID:       uuid.New(),
		TenantID: vpc1TenantID,
		Name:     "vpc-1",
		Status:   cdbm.VpcStatusReady,
	}
	dbTenant := &cdbm.Tenant{
		ID:             vpc1TenantID,
		Org:            "test-org",
		OrgDisplayName: util.GetPtr("Org Display name"),
	}

	t.Run("ok when db model is provided", func(t *testing.T) {
		summary := NewAPIVpcPeeringVpcSummary(dbVpc, dbTenant)
		assert.NotNil(t, summary)
		assert.Equal(t, dbVpc.ID.String(), summary.ID)
		assert.Equal(t, dbVpc.Name, summary.Name)
		assert.Equal(t, dbVpc.TenantID.String(), summary.TenantID)
		assert.Equal(t, dbVpc.Status, summary.Status)
		require.NotNil(t, summary.Tenant)
		assert.Equal(t, vpc1TenantID.String(), summary.Tenant.ID)
	})

	t.Run("ok when tenant is nil", func(t *testing.T) {
		summary := NewAPIVpcPeeringVpcSummary(dbVpc, nil)
		assert.NotNil(t, summary)
		assert.Nil(t, summary.Tenant)
	})

	t.Run("returns nil when db model is nil", func(t *testing.T) {
		summary := NewAPIVpcPeeringVpcSummary(nil, nil)
		assert.Nil(t, summary)
	})
}
