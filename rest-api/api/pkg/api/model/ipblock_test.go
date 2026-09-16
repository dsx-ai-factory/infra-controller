// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	ipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPIIPBlockCreateRequest_Validate(t *testing.T) {
	prefLen := 24
	prefLen33 := 33
	prefLen0 := 0
	prefLen129 := 129
	prefLen19 := 19

	tests := []struct {
		desc           string
		obj            APIIPBlockCreateRequest
		expectErr      bool
		expectedPrefix string
	}{
		{
			desc:      "error when Name is not provided",
			obj:       APIIPBlockCreateRequest{Description: cutil.GetPtr("ab"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APIIPBlockCreateRequest{Name: "a", Description: cutil.GetPtr("ab"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "ok when description is empty",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr(""), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: false,
		},
		{
			desc:      "error when SiteID is not valid uuid",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: "baduuid", RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when RoutingType is not valid",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: "unknown routing type", Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when Prefix is not valid ip address",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "bad-ip-address", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when Prefix is not valid ipv4 address",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "::2001", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when dotted-tail IPv6 prefix is declared IPv4",
			obj:       APIIPBlockCreateRequest{Name: "ab", SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "2001:db8::0.0.0.0", PrefixLength: 32, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when Prefix is not valid ipv6 address",
			obj:       APIIPBlockCreateRequest{Name: "ab", SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV6},
			expectErr: true,
		},
		{
			desc:           "ok with expanded uppercase IPv6 network address",
			obj:            APIIPBlockCreateRequest{Name: "ab", SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "2001:0DB8:0:0:0:0:0:0", PrefixLength: 64, ProtocolVersion: cdbm.IPBlockProtocolVersionV6},
			expectedPrefix: "2001:db8::",
		},
		{
			desc:      "error when expanded IPv6 prefix has nonzero host bits",
			obj:       APIIPBlockCreateRequest{Name: "ab", SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "2001:0DB8:0:0:0:0:0:1", PrefixLength: 64, ProtocolVersion: cdbm.IPBlockProtocolVersionV6},
			expectErr: true,
		},
		{
			desc:      "error when min PrefixLength is not valid for IPv4",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen0, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when max PrefixLength is not valid for ipv4",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen33, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when min PrefixLength is not valid for IPv6",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "2001:aabb::0", PrefixLength: prefLen0, ProtocolVersion: cdbm.IPBlockProtocolVersionV6},
			expectErr: true,
		},
		{
			desc:      "error when max PrefixLength is not valid for ipv6",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "2001:aabb::0", PrefixLength: prefLen129, ProtocolVersion: cdbm.IPBlockProtocolVersionV6},
			expectErr: true,
		},
		{
			desc:      "error when max prefix does not match block size",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "100.100.11.11", PrefixLength: prefLen19, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "error when ProtocolVersion is invalid",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: "ipv4"},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is not specified",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: true,
		},
		{
			desc:      "ok when all fields are speced with prefixLength",
			obj:       APIIPBlockCreateRequest{Name: "ab", Description: cutil.GetPtr("abc"), SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: false,
		},
		{
			desc:      "ok when not required fields are nil",
			obj:       APIIPBlockCreateRequest{Name: "ab", SiteID: uuid.New().String(), RoutingType: cdbm.IPBlockRoutingTypePublic, Prefix: "192.164.10.0", PrefixLength: prefLen, ProtocolVersion: cdbm.IPBlockProtocolVersionV4},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			expectedPrefix := tc.expectedPrefix
			if expectedPrefix == "" {
				expectedPrefix = tc.obj.Prefix
			}
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			assert.Equal(t, expectedPrefix, tc.obj.Prefix)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIIPBlockUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIIPBlockUpdateRequest
		expectErr bool
	}{
		{
			desc:      "ok when Name is not provided",
			obj:       APIIPBlockUpdateRequest{Description: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "ok when Description is not provided",
			obj:       APIIPBlockUpdateRequest{Name: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "error when Name is provided but is empty",
			obj:       APIIPBlockUpdateRequest{Name: cutil.GetPtr(""), Description: cutil.GetPtr("ab")},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APIIPBlockUpdateRequest{Name: cutil.GetPtr("a"), Description: cutil.GetPtr("ab")},
			expectErr: true,
		},
		{
			desc:      "ok when description is not valid with empty",
			obj:       APIIPBlockUpdateRequest{Name: cutil.GetPtr("ab"), Description: cutil.GetPtr("")},
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

func TestAPIIPBlockNew(t *testing.T) {
	dbObj := &cdbm.IPBlock{
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
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbObj.ID.String(),
			Status:   cdbm.TenantAccountStatusPending,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	msh := []APIStatusDetail{}
	for _, s := range dbsds {
		msh = append(msh, NewAPIStatusDetail(s))
	}
	tests := []struct {
		desc       string
		dbObj      *cdbm.IPBlock
		sdObj      []cdbm.StatusDetail
		apiObj     *APIIPBlock
		dbUsageObj *ipam.Usage
	}{
		{
			desc:  "test creating API IPBlock",
			dbObj: dbObj,
			sdObj: dbsds,
			dbUsageObj: &ipam.Usage{
				AvailableIPs:              1,
				AvailablePrefixes:         nil,
				AcquiredIPs:               0,
				AcquiredPrefixes:          0,
				AvailableSmallestPrefixes: 0,
			},
			apiObj: &APIIPBlock{
				ID:                       dbObj.ID.String(),
				Name:                     dbObj.Name,
				Description:              dbObj.Description,
				SiteID:                   dbObj.SiteID.String(),
				InfrastructureProviderID: dbObj.InfrastructureProviderID.String(),
				TenantID:                 util.GetUUIDPtrToStrPtr(dbObj.TenantID),
				RoutingType:              dbObj.RoutingType,
				Prefix:                   dbObj.Prefix,
				PrefixLength:             dbObj.PrefixLength,
				ProtocolVersion:          dbObj.ProtocolVersion,
				UsageStats: &APIIPBlockUsageStats{
					AvailableIPs:              1,
					AvailablePrefixes:         nil,
					AcquiredIPs:               0,
					AcquiredPrefixes:          0,
					AvailableSmallestPrefixes: 0,
				},
				Status:        dbObj.Status,
				StatusHistory: msh,
				Created:       dbObj.Created,
				Updated:       dbObj.Updated,
			},
		},
		{
			desc:       "test creating API IPBlock nil usage stats",
			dbObj:      dbObj,
			sdObj:      dbsds,
			dbUsageObj: nil,
			apiObj: &APIIPBlock{
				ID:                       dbObj.ID.String(),
				Name:                     dbObj.Name,
				Description:              dbObj.Description,
				SiteID:                   dbObj.SiteID.String(),
				InfrastructureProviderID: dbObj.InfrastructureProviderID.String(),
				TenantID:                 util.GetUUIDPtrToStrPtr(dbObj.TenantID),
				RoutingType:              dbObj.RoutingType,
				Prefix:                   dbObj.Prefix,
				PrefixLength:             dbObj.PrefixLength,
				ProtocolVersion:          dbObj.ProtocolVersion,
				UsageStats:               nil,
				Status:                   dbObj.Status,
				StatusHistory:            msh,
				Created:                  dbObj.Created,
				Updated:                  dbObj.Updated,
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIIPBlock(tc.dbObj, tc.sdObj, tc.dbUsageObj)
			assert.Equal(t, tc.apiObj, got)
		})
	}
}

func TestAPIIPBlockNewIPBlockSummary(t *testing.T) {
	dbObj := &cdbm.IPBlock{
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
	tests := []struct {
		desc   string
		dbObj  *cdbm.IPBlock
		apiObj *APIIPBlockSummary
	}{
		{
			desc:  "test creating API IPBlockSummary",
			dbObj: dbObj,
			apiObj: &APIIPBlockSummary{
				ID:           dbObj.ID.String(),
				Name:         dbObj.Name,
				RoutingType:  dbObj.RoutingType,
				Prefix:       dbObj.Prefix,
				PrefixLength: dbObj.PrefixLength,
				Status:       dbObj.Status,
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIIPBlockSummary(tc.dbObj)
			assert.Equal(t, tc.apiObj, got)
		})
	}
}
