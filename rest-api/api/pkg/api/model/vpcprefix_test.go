// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"errors"
	"fmt"
	"testing"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	"github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/vpcprefix"
	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	ipam "github.com/dsx-ai-factory/infra-controller/rest-api/ipam"
)

func TestAPIVpcPrefixCreateRequest_Validate(t *testing.T) {
	prefixBelowMinimum := vpcprefix.PrefixLengthMinimum - 1
	validPrefixLength := 24
	prefixAtMaximum := vpcprefix.PrefixLengthMaximum
	prefixAboveMaximum := vpcprefix.PrefixLengthMaximum + 1
	vpcID := uuid.New().String()
	ipBlockID := uuid.New().String()
	tests := []struct {
		desc                    string
		obj                     APIVpcPrefixCreateRequest
		expectErr               bool
		expectedError           string
		expectedCanonicalPrefix string
	}{
		{
			desc:      "error when Name is not provided",
			obj:       APIVpcPrefixCreateRequest{VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: &validPrefixLength},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APIVpcPrefixCreateRequest{Name: "a", VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: &validPrefixLength},
			expectErr: true,
		},
		{
			desc:      "ok with automatic allocation",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: &validPrefixLength},
			expectErr: false,
		},
		{
			desc:      "error when VpcID is not valid uuid",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: "baduuid", IPBlockID: &ipBlockID, PrefixLength: &validPrefixLength},
			expectErr: true,
		},
		{
			desc:      "error when IPBlockID is not valid uuid",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: cutil.GetPtr("bad"), PrefixLength: &validPrefixLength},
			expectErr: true,
		},
		{
			desc:      "error when IPBlockID is not provided",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, PrefixLength: &validPrefixLength},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is not valid < min",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: &prefixBelowMinimum},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is not valid > max",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: &prefixAboveMaximum},
			expectErr: true,
		},
		{
			desc:      "error when prefixLength is zero",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: cutil.GetPtr(0)},
			expectErr: true,
		},
		{
			desc:      "error when both allocation selectors are specified",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("10.20.0.0/24"), PrefixLength: &validPrefixLength},
			expectErr: true,
		},
		{
			desc:      "error when neither allocation selector is specified",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID},
			expectErr: true,
		},
		{
			desc:      "error when prefix is empty",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("")},
			expectErr: true,
		},
		{
			desc:      "error when prefix has no length",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("10.20.0.0")},
			expectErr: true,
		},
		{
			desc:      "error when prefix has host bits",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("10.20.30.7/24")},
			expectErr: true,
		},
		{
			desc:      "error when prefix is IPv4-mapped IPv6",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("::ffff:10.20.0.0/120")},
			expectErr: true,
		},
		{
			desc:          "error when explicit prefix length is below structural minimum",
			obj:           APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("10.0.0.0/7")},
			expectErr:     true,
			expectedError: `prefix "10.0.0.0/7" has prefix length 7; must be between 8 and 126`,
		},
		{
			desc:          "error when explicit prefix length is above structural maximum",
			obj:           APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("2001:db8::/127")},
			expectErr:     true,
			expectedError: `prefix "2001:db8::/127" has prefix length 127; must be between 8 and 126`,
		},
		{
			desc:                    "ok with explicit IPv4 prefix",
			obj:                     APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("10.20.0.0/24")},
			expectedCanonicalPrefix: "10.20.0.0/24",
		},
		{
			desc:                    "canonicalizes explicit IPv6 prefix",
			obj:                     APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, Prefix: cutil.GetPtr("2001:0db8:0000:0000:0000:0000:0000:0000/64")},
			expectedCanonicalPrefix: "2001:db8::/64",
		},
		{
			desc:      "ok at structural maximum",
			obj:       APIVpcPrefixCreateRequest{Name: "ab", VpcID: vpcID, IPBlockID: &ipBlockID, PrefixLength: &prefixAtMaximum},
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
			if tc.expectedError != "" {
				require.ErrorContains(t, err, tc.expectedError)
			}
			if tc.expectedCanonicalPrefix != "" {
				require.NotNil(t, tc.obj.Prefix)
				assert.Equal(t, tc.expectedCanonicalPrefix, *tc.obj.Prefix)
			}
		})
	}

	nullSelectorTests := []struct {
		name      string
		body      string
		expectErr bool
	}{
		{
			name: "null prefix is omitted for automatic allocation",
			body: fmt.Sprintf(`{"name":"ab","vpcId":%q,"ipBlockId":%q,"prefix":null,"prefixLength":24}`, vpcID, ipBlockID),
		},
		{
			name: "null prefixLength is omitted for explicit allocation",
			body: fmt.Sprintf(`{"name":"ab","vpcId":%q,"ipBlockId":%q,"prefix":"10.20.0.0/24","prefixLength":null}`, vpcID, ipBlockID),
		},
		{
			name:      "both null selectors are rejected",
			body:      fmt.Sprintf(`{"name":"ab","vpcId":%q,"ipBlockId":%q,"prefix":null,"prefixLength":null}`, vpcID, ipBlockID),
			expectErr: true,
		},
	}
	for _, test := range nullSelectorTests {
		t.Run(test.name, func(t *testing.T) {
			var request APIVpcPrefixCreateRequest
			require.NoError(t, json.Unmarshal([]byte(test.body), &request))
			err := request.Validate()
			assert.Equal(t, test.expectErr, err != nil)
		})
	}
}

// TestAPIVpcPrefixCreateRequest_ValidatePrefixLength verifies the resolved
// maximum is returned as a field validation error.
func TestAPIVpcPrefixCreateRequest_ValidatePrefixLength(t *testing.T) {
	tests := []struct {
		name          string
		prefixLength  *int
		prefix        *string
		maximumLength int
		expectErr     bool
		errorField    string
	}{
		{
			name:          "stateful IPv6 accepts /126",
			prefixLength:  cutil.GetPtr(vpcprefix.PrefixLengthMaximum),
			maximumLength: vpcprefix.IPv6StatefulPrefixLengthMaximum,
		},
		{
			name:          "SLAAC IPv6 rejects /64",
			prefixLength:  cutil.GetPtr(vpcprefix.IPv6SLAACPrefixLengthMaximum + 1),
			maximumLength: vpcprefix.IPv6SLAACPrefixLengthMaximum,
			expectErr:     true,
			errorField:    "prefixLength",
		},
		{
			name:          "SLAAC IPv6 accepts explicit /63",
			prefix:        cutil.GetPtr("2001:db8::/63"),
			maximumLength: vpcprefix.IPv6SLAACPrefixLengthMaximum,
		},
		{
			name:          "SLAAC IPv6 rejects explicit /64",
			prefix:        cutil.GetPtr("2001:db8::/64"),
			maximumLength: vpcprefix.IPv6SLAACPrefixLengthMaximum,
			expectErr:     true,
			errorField:    "prefix",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			req := APIVpcPrefixCreateRequest{Prefix: tc.prefix, PrefixLength: tc.prefixLength}
			err := req.ValidatePrefixLength(tc.maximumLength)
			assert.Equal(t, tc.expectErr, err != nil)
			if !tc.expectErr {
				return
			}

			var validationErrors validation.Errors
			require.True(t, errors.As(err, &validationErrors))
			assert.Error(t, validationErrors[tc.errorField])
		})
	}
}

func TestAPIVpcPrefixUpdateRequest_Validate(t *testing.T) {
	prefix24 := 24
	tests := []struct {
		desc      string
		obj       APIVpcPrefixUpdateRequest
		expectErr bool
	}{
		{
			desc:      "ok when Name is not provided",
			obj:       APIVpcPrefixUpdateRequest{IPBlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: cutil.GetPtr(prefix24)},
			expectErr: true,
		},
		{
			desc:      "ok when ipbock is not provided",
			obj:       APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "error when Name is provided but is empty",
			obj:       APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("")},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("a")},
			expectErr: true,
		},
		{
			desc:      "ok when ipblock provided but not prefix length",
			obj:       APIVpcPrefixUpdateRequest{IPBlockID: cutil.GetPtr(uuid.New().String())},
			expectErr: true,
		},
		{
			desc:      "ok when prefix length provided but not ipblock",
			obj:       APIVpcPrefixUpdateRequest{PrefixLength: cutil.GetPtr(prefix24)},
			expectErr: true,
		},
		{
			desc:      "error when prefix is provided",
			obj:       APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("renamed"), Prefix: cutil.GetPtr("10.20.0.0/24")},
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

func TestAPIVpcPrefixNew(t *testing.T) {
	ipBlock := &cdbm.IPBlock{
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
	dbObj1 := &cdbm.VpcPrefix{
		ID:           uuid.New(),
		Name:         "test",
		SiteID:       uuid.New(),
		VpcID:        uuid.New(),
		IPBlockID:    &ipBlock.ID,
		Prefix:       ipBlock.Prefix,
		PrefixLength: 24,
		Created:      cdb.GetCurTime(),
		Updated:      cdb.GetCurTime(),
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbObj1.ID.String(),
			Status:   cdbm.VpcPrefixStatusReady,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	tests := []struct {
		desc   string
		dbObj  *cdbm.VpcPrefix
		prefix *string
		sdObj  []cdbm.StatusDetail
	}{
		{
			desc:   "test creating API VpcPrefix only IPv4",
			dbObj:  dbObj1,
			prefix: cutil.GetPtr("192.168.0.0"),
			sdObj:  dbsds,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIVpcPrefix(tc.dbObj, tc.sdObj, nil)
			assert.Equal(t, tc.dbObj.ID.String(), got.ID)
			assert.NotNil(t, tc.dbObj.SiteID)
			assert.NotNil(t, tc.dbObj.VpcID)
			assert.Equal(t, tc.dbObj.Prefix, *got.Prefix)
			assert.Equal(t, tc.dbObj.PrefixLength, got.PrefixLength)
			assert.Nil(t, got.UsageStats)
		})
	}
}

func TestNewAPIVpcPrefix_UsageStats(t *testing.T) {
	dbObj := &cdbm.VpcPrefix{
		ID:           uuid.New(),
		Name:         "vpc-prefix-stats",
		SiteID:       uuid.New(),
		VpcID:        uuid.New(),
		IPBlockID:    cutil.GetPtr(uuid.New()),
		Prefix:       "10.1.0.0",
		PrefixLength: 24,
		Created:      cdb.GetCurTime(),
		Updated:      cdb.GetCurTime(),
	}
	tests := []struct {
		desc  string
		usage *ipam.Usage
		want  *APIIPBlockUsageStats
	}{
		{
			desc:  "non-nil empty usage yields zero-valued UsageStats",
			usage: &ipam.Usage{},
			want: &APIIPBlockUsageStats{
				AvailablePrefixes: []string(nil),
			},
		},
		{
			desc: "partial usage copies only populated fields",
			usage: &ipam.Usage{
				AvailableIPs:      100,
				AvailablePrefixes: []string{"10.1.1.0/26"},
			},
			want: &APIIPBlockUsageStats{
				AvailableIPs:      100,
				AvailablePrefixes: []string{"10.1.1.0/26"},
			},
		},
		{
			desc: "full usage maps all fields",
			usage: &ipam.Usage{
				AvailableIPs:              50,
				AcquiredIPs:               14,
				AvailableSmallestPrefixes: 200,
				AvailablePrefixes:         []string{"10.2.0.0/26", "10.2.0.64/26"},
				AcquiredPrefixes:          8,
			},
			want: &APIIPBlockUsageStats{
				AvailableIPs:              50,
				AcquiredIPs:               14,
				AvailableSmallestPrefixes: 200,
				AvailablePrefixes:         []string{"10.2.0.0/26", "10.2.0.64/26"},
				AcquiredPrefixes:          8,
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIVpcPrefix(dbObj, nil, tc.usage)
			req := assert.New(t)
			req.NotNil(got.UsageStats)
			req.Equal(tc.want.AvailableIPs, got.UsageStats.AvailableIPs)
			req.Equal(tc.want.AcquiredIPs, got.UsageStats.AcquiredIPs)
			req.Equal(tc.want.AvailablePrefixes, got.UsageStats.AvailablePrefixes)
			req.Equal(tc.want.AvailableSmallestPrefixes, got.UsageStats.AvailableSmallestPrefixes)
			req.Equal(tc.want.AcquiredPrefixes, got.UsageStats.AcquiredPrefixes)
		})
	}
}

func TestAPIVpcPrefixCreateRequest_ToProto(t *testing.T) {
	prefixID := uuid.New()
	vpcID := uuid.New()
	vp := &cdbm.VpcPrefix{ID: prefixID, Name: "prefix-a", Prefix: "10.0.0.0/16"}
	vpc := &cdbm.Vpc{ID: vpcID}

	t.Run("sources canonical fields from the entity's ToProto", func(t *testing.T) {
		apiReq := APIVpcPrefixCreateRequest{
			Name:         "prefix-a",
			VpcID:        vpcID.String(),
			IPBlockID:    cutil.GetPtr(uuid.New().String()),
			PrefixLength: cutil.GetPtr(24),
		}
		req := apiReq.ToProto(vp, vpc)

		require.NotNil(t, req)
		require.NotNil(t, req.Id)
		assert.Equal(t, prefixID.String(), req.Id.Value)
		require.NotNil(t, req.VpcId)
		assert.Equal(t, vpcID.String(), req.VpcId.Value)
		require.NotNil(t, req.Config)
		assert.Equal(t, "10.0.0.0/16", req.Config.Prefix)
		require.NotNil(t, req.Metadata)
		assert.Equal(t, "prefix-a", req.Metadata.Name)
	})
}

func TestAPIVpcPrefixUpdateRequest_ToProto(t *testing.T) {
	prefixID := uuid.New()
	vp := &cdbm.VpcPrefix{ID: prefixID, Name: "prefix-a"}

	t.Run("sources Id and Metadata.Name from the post-merge entity", func(t *testing.T) {
		apiReq := APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("prefix-a")}
		req := apiReq.ToProto(vp)
		require.NotNil(t, req)
		require.NotNil(t, req.Id)
		assert.Equal(t, prefixID.String(), req.Id.Value)
		require.NotNil(t, req.Metadata)
		assert.Equal(t, "prefix-a", req.Metadata.Name)
	})
}
