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

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestAPISubnetCreateRequest_Validate(t *testing.T) {
	prefix7 := 7
	prefix24 := 24
	prefix32 := 32
	prefix31 := 31
	tests := []struct {
		desc           string
		obj            APISubnetCreateRequest
		expectErr      bool
		expectedErrMsg string
	}{
		{
			desc:      "error when Name is not provided",
			obj:       APISubnetCreateRequest{Description: cutil.GetPtr("ab"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix24},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APISubnetCreateRequest{Name: "a", Description: cutil.GetPtr("ab"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix24},
			expectErr: true,
		},
		{
			desc:      "ok when description is empty",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr(""), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix24},
			expectErr: false,
		},
		{
			desc:      "error when VpcID is not valid uuid",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: "baduuid", IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix24},
			expectErr: true,
		},
		{
			desc:      "error when IPv4Block is not valid uuid",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr("bad"), PrefixLength: prefix24},
			expectErr: true,
		},
		{
			desc:           "REST Subnet creation rejects ipv6BlockId",
			obj:            APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), IPv6BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix24},
			expectErr:      true,
			expectedErrMsg: "ipv6BlockId: " + validationErrorIPv6SubnetNotSupported + ".",
		},
		{
			desc:      "REST Subnet creation requires ipv4BlockId",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), PrefixLength: prefix24},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is not valid < min",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix7},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is not valid > max",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix31},
			expectErr: true,
		},
		{
			desc:      "ok when all fields are specified",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix24},
			expectErr: false,
		},
		{
			desc:      "error when /32 subnet is created",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefix32},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is not specified",
			obj:       APISubnetCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(uuid.New().String())},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				if tc.expectedErrMsg != "" {
					assert.Equal(t, tc.expectedErrMsg, err.Error())
				}
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPISubnetCreateRequest_ToProto(t *testing.T) {
	subID := uuid.New()
	vpcID := uuid.New()
	domainID := uuid.New()
	prefix := "10.0.0.0"
	gateway := "10.0.0.1"
	mtu := 9000

	subnet := &cdbm.Subnet{
		ID:           subID,
		VpcID:        vpcID,
		Name:         "subnet-a",
		DomainID:     &domainID,
		IPv4Prefix:   &prefix,
		IPv4Gateway:  &gateway,
		PrefixLength: 16,
		MTU:          &mtu,
	}
	vpc := &cdbm.Vpc{ID: vpcID}

	t.Run("sources canonical fields from the entity's ToProto and overlays the reservedIPCount", func(t *testing.T) {
		scr := APISubnetCreateRequest{
			Name:         "subnet-a",
			VpcID:        vpcID.String(),
			IPv4BlockID:  cutil.GetPtr(uuid.New().String()),
			PrefixLength: 16,
		}
		req := scr.ToProto(subnet, vpc, 2)
		require.NotNil(t, req)
		require.NotNil(t, req.Id)
		assert.Equal(t, subID.String(), req.Id.Value)
		assert.Equal(t, "subnet-a", req.Name)
		require.NotNil(t, req.SubdomainId)
		assert.Equal(t, domainID.String(), req.SubdomainId.Value)
		require.NotNil(t, req.VpcId)
		assert.Equal(t, vpcID.String(), req.VpcId.Value)
		require.NotNil(t, req.Mtu)
		assert.Equal(t, int32(9000), *req.Mtu)
		require.Len(t, req.Prefixes, 1)
		assert.Equal(t, "10.0.0.0/16", req.Prefixes[0].Prefix)
		require.NotNil(t, req.Prefixes[0].Gateway)
		assert.Equal(t, gateway, *req.Prefixes[0].Gateway)
		assert.Equal(t, int32(2), req.Prefixes[0].ReserveFirst)
	})

	t.Run("uses VPC's Site-facing ID for the parent ID", func(t *testing.T) {
		ctrlID := uuid.New()
		vpcWithCtrl := &cdbm.Vpc{ID: vpcID, ControllerVpcID: &ctrlID}
		scr := APISubnetCreateRequest{}
		req := scr.ToProto(subnet, vpcWithCtrl, 2)
		require.NotNil(t, req.VpcId)
		assert.Equal(t, ctrlID.String(), req.VpcId.Value)
	})
}

func TestAPISubnetUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APISubnetUpdateRequest
		expectErr bool
	}{
		{
			desc:      "ok when Name is not provided",
			obj:       APISubnetUpdateRequest{Description: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "ok when Description is not provided",
			obj:       APISubnetUpdateRequest{Name: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "error when Name is provided but is empty",
			obj:       APISubnetUpdateRequest{Name: cutil.GetPtr(""), Description: cutil.GetPtr("ab")},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APISubnetUpdateRequest{Name: cutil.GetPtr("a"), Description: cutil.GetPtr("ab")},
			expectErr: true,
		},
		{
			desc:      "ok when description is not valid with empty",
			obj:       APISubnetUpdateRequest{Name: cutil.GetPtr("ab"), Description: cutil.GetPtr("")},
			expectErr: false,
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

func TestAPISubnetNew(t *testing.T) {
	ipv4Block := &cdbm.IPBlock{
		ID:                       uuid.New(),
		Name:                     "test",
		SiteID:                   uuid.New(),
		InfrastructureProviderID: uuid.New(),
		TenantID:                 cutil.GetPtr(uuid.New()),
		RoutingType:              cdbm.IPBlockRoutingTypePublic,
		Prefix:                   "192.168.0.0",
		PrefixLength:             16,
		ProtocolVersion:          "IPv4",
		Status:                   cdbm.IPBlockStatusPending,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}
	ipv6Block := &cdbm.IPBlock{
		ID:                       uuid.New(),
		Name:                     "test",
		SiteID:                   uuid.New(),
		InfrastructureProviderID: uuid.New(),
		TenantID:                 cutil.GetPtr(uuid.New()),
		RoutingType:              cdbm.IPBlockRoutingTypePublic,
		Prefix:                   "2001:aabb::",
		PrefixLength:             16,
		ProtocolVersion:          "IPv6",
		Status:                   cdbm.IPBlockStatusPending,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}
	dbObj := &cdbm.Subnet{
		ID:                         uuid.New(),
		Name:                       "test",
		Description:                cutil.GetPtr("test"),
		SiteID:                     uuid.New(),
		VpcID:                      uuid.New(),
		TenantID:                   uuid.New(),
		ControllerNetworkSegmentID: cutil.GetPtr(uuid.New()),
		IPv4BlockID:                &ipv4Block.ID,
		IPv4Prefix:                 &ipv4Block.Prefix,
		IPv6BlockID:                &ipv6Block.ID,
		IPv6Prefix:                 &ipv6Block.Prefix,
		Status:                     cdbm.SubnetStatusPending,
		Created:                    cdb.GetCurTime(),
		Updated:                    cdb.GetCurTime(),
	}
	dbObj1 := &cdbm.Subnet{
		ID:          uuid.New(),
		Name:        "test",
		Description: cutil.GetPtr("test"),
		SiteID:      uuid.New(),
		VpcID:       uuid.New(),
		TenantID:    uuid.New(),
		IPv4BlockID: &ipv4Block.ID,
		IPv4Prefix:  &ipv4Block.Prefix,
		Status:      cdbm.SubnetStatusPending,
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbObj.ID.String(),
			Status:   cdbm.TenantAccountStatusPending,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	tests := []struct {
		desc        string
		dbObj       *cdbm.Subnet
		ipbv4Prefix *string
		ipbv6Prefix *string
		gwv4        *string
		gwv6        *string
		sdObj       []cdbm.StatusDetail
	}{
		{
			desc:        "test creating API Subnet both IPv4 and IPv6",
			dbObj:       dbObj,
			ipbv4Prefix: cutil.GetPtr("192.168.0.0"),
			ipbv6Prefix: cutil.GetPtr("2001:aabb::"),
			gwv4:        cutil.GetPtr("192.168.0.1"),
			gwv6:        cutil.GetPtr("2001:aabb::0::1"),
			sdObj:       dbsds,
		},
		{
			desc:        "test creating API Subnet only IPv4",
			dbObj:       dbObj1,
			ipbv4Prefix: cutil.GetPtr("192.168.0.0"),
			gwv4:        cutil.GetPtr("192.168.0.1"),
			sdObj:       dbsds,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPISubnet(tc.dbObj, tc.sdObj, nil)
			assert.Equal(t, tc.dbObj.ID.String(), got.ID)
			assert.NotNil(t, tc.dbObj.SiteID)
			if tc.dbObj.ControllerNetworkSegmentID != nil {
				assert.Equal(t, tc.dbObj.ControllerNetworkSegmentID.String(), *got.ControllerNetworkSegmentID)
			}
			assert.Equal(t, *tc.dbObj.Description, *got.Description)
			assert.Equal(t, *tc.dbObj.IPv4Prefix, *got.IPv4Prefix)
			if tc.dbObj.IPv6Prefix != nil {
				assert.Equal(t, *tc.dbObj.IPv6Prefix, *got.IPv6Prefix)
			}

			assert.Equal(t, tc.dbObj.PrefixLength, got.PrefixLength)
		})
	}
}
