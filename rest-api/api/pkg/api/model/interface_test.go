// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"reflect"
	"testing"
	"time"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPIInterface(t *testing.T) {
	type args struct {
		dbis *cdbm.Interface
	}

	dbis := &cdbm.Interface{
		ID:                 uuid.New(),
		InstanceID:         uuid.New(),
		SubnetID:           cutil.GetPtr(uuid.New()),
		VpcPrefixID:        nil,
		MachineInterfaceID: cutil.GetPtr(uuid.New()),
		RequestedIpAddress: cutil.GetPtr("192.0.2.10"),
		IPAddresses:        []string{"192.0.2.10"},
		IPPrefixes:         []string{"192.0.2.0/24", "2001:db8::/64"},
		Created:            time.Now(),
		Updated:            time.Now(),
	}
	vpcID := uuid.New()
	vpcFamilyMode := cdbm.InterfaceVpcIPFamilyModeDualStack
	vpcInterface := &cdbm.Interface{
		ID:                 uuid.New(),
		InstanceID:         uuid.New(),
		VpcID:              &vpcID,
		VpcIPFamilyMode:    &vpcFamilyMode,
		VpcPrefixID:        cutil.GetPtr(uuid.New()),
		RequestedIpAddress: nil,
		Created:            time.Now(),
		Updated:            time.Now(),
	}
	vpcPrefixInterface := &cdbm.Interface{
		ID:          uuid.New(),
		InstanceID:  uuid.New(),
		VpcPrefixID: cutil.GetPtr(uuid.New()),
		Created:     time.Now(),
		Updated:     time.Now(),
	}

	tests := []struct {
		name             string
		args             args
		want             *APIInterface
		wantPrefixesJSON string
	}{
		{
			name: "test new API Interface Subnet initializer",
			args: args{
				dbis: dbis,
			},
			want: &APIInterface{
				ID:                 dbis.ID.String(),
				InstanceID:         dbis.InstanceID.String(),
				SubnetID:           cutil.GetPtr(dbis.SubnetID.String()),
				RequestedIpAddress: cutil.GetPtr("192.0.2.10"),
				IPAddresses:        dbis.IPAddresses,
				IPPrefixes:         dbis.IPPrefixes,
				Status:             dbis.Status,
				Created:            dbis.Created,
				Updated:            dbis.Updated,
			},
			wantPrefixesJSON: `["192.0.2.0/24","2001:db8::/64"]`,
		},
		{
			name: "test new API Interface explicit VPC Prefix initializer",
			args: args{
				dbis: vpcPrefixInterface,
			},
			want: &APIInterface{
				ID:          vpcPrefixInterface.ID.String(),
				InstanceID:  vpcPrefixInterface.InstanceID.String(),
				VpcPrefixID: cutil.GetPtr(vpcPrefixInterface.VpcPrefixID.String()),
				IPPrefixes:  []string{},
				Status:      vpcPrefixInterface.Status,
				Created:     vpcPrefixInterface.Created,
				Updated:     vpcPrefixInterface.Updated,
			},
			wantPrefixesJSON: `[]`,
		},
		{
			name: "test new API Interface VPC selection initializer",
			args: args{
				dbis: vpcInterface,
			},
			want: &APIInterface{
				ID:         vpcInterface.ID.String(),
				InstanceID: vpcInterface.InstanceID.String(),
				VpcID:      cutil.GetPtr(vpcID.String()),
				IPFamilies: []IPFamily{IPFamilyIPv4, IPFamilyIPv6},
				IPPrefixes: []string{},
				Status:     vpcInterface.Status,
				Created:    vpcInterface.Created,
				Updated:    vpcInterface.Updated,
			},
			wantPrefixesJSON: `[]`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIInterface(tt.args.dbis)
			if !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIInterface() = %v, want %v", got, tt.want)
			}
			encoded, err := json.Marshal(got)
			require.NoError(t, err)
			var response map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(encoded, &response))
			assert.JSONEq(t, tt.wantPrefixesJSON, string(response["ipPrefixes"]))
		})
	}
}

// TestAPIInterfaceVpcSelectionRoundTrip verifies resolved prefix status does not become update intent.
func TestAPIInterfaceVpcSelectionRoundTrip(t *testing.T) {
	vpcID := uuid.New()
	resolvedVpcPrefixID := uuid.New()
	familyMode := cdbm.InterfaceVpcIPFamilyModeIPv4Only
	apiInterface := NewAPIInterface(&cdbm.Interface{
		ID:              uuid.New(),
		InstanceID:      uuid.New(),
		VpcID:           &vpcID,
		VpcIPFamilyMode: &familyMode,
		VpcPrefixID:     &resolvedVpcPrefixID,
		VpcPrefix:       &cdbm.VpcPrefix{ID: resolvedVpcPrefixID},
	})
	assert.Nil(t, apiInterface.VpcPrefixID)
	assert.Nil(t, apiInterface.VpcPrefix)

	data, err := json.Marshal(apiInterface)
	require.NoError(t, err)

	var updateRequest APIInterfaceCreateOrUpdateRequest
	require.NoError(t, json.Unmarshal(data, &updateRequest))
	require.NotNil(t, updateRequest.VpcID)
	assert.Equal(t, vpcID.String(), *updateRequest.VpcID)
	assert.Equal(t, []IPFamily{IPFamilyIPv4}, updateRequest.IPFamilies)
	assert.Nil(t, updateRequest.VpcPrefixID)
	assert.NoError(t, updateRequest.Validate())
}

func TestAPIInterfaceCreateOrUpdateRequest_InlineRoutingProfileValidate(t *testing.T) {
	tests := []struct {
		name                       string
		req                        APIInterfaceCreateOrUpdateRequest
		wantErr                    bool
		wantErrorContains          []string
		wantAllowedAnycastPrefixes []string
		wantNonNilPrefixes         bool
	}{
		{
			name: "VPC Prefix interface accepts IPv4 and IPv6 anycast prefixes",
			req: APIInterfaceCreateOrUpdateRequest{
				VpcPrefixID: cutil.GetPtr(uuid.NewString()),
				InlineRoutingProfile: &APIInterfaceInlineRoutingProfile{
					AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
				},
			},
			wantAllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
		},
		{
			name: "explicit empty routing profile stays non-nil with empty anycast prefixes",
			req: APIInterfaceCreateOrUpdateRequest{
				VpcPrefixID:          cutil.GetPtr(uuid.NewString()),
				InlineRoutingProfile: &APIInterfaceInlineRoutingProfile{},
			},
			wantAllowedAnycastPrefixes: []string{},
			wantNonNilPrefixes:         true,
		},
		{
			name: "invalid anycast prefix returns field-specific error",
			req: APIInterfaceCreateOrUpdateRequest{
				VpcPrefixID: cutil.GetPtr(uuid.NewString()),
				InlineRoutingProfile: &APIInterfaceInlineRoutingProfile{
					AllowedAnycastPrefixes: []string{"not-a-prefix"},
				},
			},
			wantErr:           true,
			wantErrorContains: []string{"allowedAnycastPrefixes", "not-a-prefix"},
		},
		{
			name: "Subnet interface rejects routing profile",
			req: APIInterfaceCreateOrUpdateRequest{
				SubnetID:             cutil.GetPtr(uuid.NewString()),
				InlineRoutingProfile: &APIInterfaceInlineRoutingProfile{},
			},
			wantErr:           true,
			wantErrorContains: []string{"inlineRoutingProfile", "cannot be specified for Subnet based Interfaces"},
		},
		{
			name: "Subnet interface accepts nil routing profile",
			req: APIInterfaceCreateOrUpdateRequest{
				SubnetID: cutil.GetPtr(uuid.NewString()),
			},
		},
		{
			name: "VPC Prefix interface accepts nil routing profile",
			req: APIInterfaceCreateOrUpdateRequest{
				VpcPrefixID: cutil.GetPtr(uuid.NewString()),
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				require.Error(t, err)
				for _, want := range tt.wantErrorContains {
					assert.Contains(t, err.Error(), want)
				}
				return
			}

			assert.NoError(t, err)
			if tt.req.InlineRoutingProfile != nil {
				if tt.wantNonNilPrefixes {
					assert.NotNil(t, tt.req.InlineRoutingProfile.AllowedAnycastPrefixes)
				}
				assert.Equal(t, tt.wantAllowedAnycastPrefixes, tt.req.InlineRoutingProfile.AllowedAnycastPrefixes)
			}
		})
	}
}

func TestAPIInterfaceInlineRoutingProfile_ToDB(t *testing.T) {
	var nilProfile *APIInterfaceInlineRoutingProfile
	assert.Nil(t, nilProfile.ToDB())

	emptyProfile := (&APIInterfaceInlineRoutingProfile{}).ToDB()
	require.NotNil(t, emptyProfile)
	assert.NotNil(t, emptyProfile.AllowedAnycastPrefixes)
	assert.Empty(t, emptyProfile.AllowedAnycastPrefixes)

	apiProfile := &APIInterfaceInlineRoutingProfile{
		AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
	}
	dbProfile := apiProfile.ToDB()
	require.NotNil(t, dbProfile)
	assert.Equal(t, []string{"192.0.2.0/24", "2001:db8::/64"}, dbProfile.AllowedAnycastPrefixes)

	apiProfile.AllowedAnycastPrefixes[0] = "198.51.100.0/24"
	assert.Equal(t, []string{"192.0.2.0/24", "2001:db8::/64"}, dbProfile.AllowedAnycastPrefixes)
}

func TestAPIInterfaceInlineRoutingProfile_FromDB(t *testing.T) {
	dbProfile := &cdbm.InterfaceInlineRoutingProfile{
		AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
	}
	apiProfile := &APIInterfaceInlineRoutingProfile{}
	apiProfile.FromDB(dbProfile)
	assert.Equal(t, []string{"192.0.2.0/24", "2001:db8::/64"}, apiProfile.AllowedAnycastPrefixes)

	dbProfile.AllowedAnycastPrefixes[0] = "198.51.100.0/24"
	assert.Equal(t, []string{"192.0.2.0/24", "2001:db8::/64"}, apiProfile.AllowedAnycastPrefixes)

	var nilProfile *APIInterfaceInlineRoutingProfile
	nilProfile.FromDB(dbProfile)
}

func TestAPIInterfaceCreateRequest_Validate(t *testing.T) {
	type fields struct {
		SubnetID    *string
		VpcPrefixID *string
		VpcID       *string
		IPFamilies  []IPFamily
		IPAddress   *string
		IsPhysical  bool
		Device      *string

		DeviceInstance    *int
		VirtualFunctionID *int
	}
	tests := []struct {
		name             string
		fields           fields
		wantErr          bool
		wantErrorMessage string
		wantIPFamilies   []IPFamily
	}{
		{
			name: "test valid Interface Subnet request",
			fields: fields{
				SubnetID: cutil.GetPtr(uuid.New().String()),
			},
			wantErr: false,
		},
		{
			name: "test valid Interface VpcPrefix request",
			fields: fields{
				VpcPrefixID: cutil.GetPtr(uuid.New().String()),
				IPAddress:   cutil.GetPtr("192.0.2.11"),
				IsPhysical:  true,
			},
			wantErr: false,
		},
		{
			name: "test valid Interface VPC request",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{IPFamilyIPv4},
				IsPhysical: true,
			},
			wantErr: false,
		},
		{
			name: "test valid Interface VPC request normalizes duplicate IP families",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{IPFamilyIPv6, IPFamilyIPv4, IPFamilyIPv6, IPFamilyIPv4},
			},
			wantErr:        false,
			wantIPFamilies: []IPFamily{IPFamilyIPv4, IPFamilyIPv6},
		},
		{
			name: "test invalid Interface Subnet request",
			fields: fields{
				SubnetID: cutil.GetPtr("bad-uuid"),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface VpcPrefix request",
			fields: fields{
				VpcPrefixID: cutil.GetPtr("bad-uuid"),
				IsPhysical:  true,
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface VPC UUID",
			fields: fields{
				VpcID: cutil.GetPtr("bad-uuid"),
			},
			wantErr:          true,
			wantErrorMessage: validationErrorInvalidUUID,
		},
		{
			name: "test invalid Interface request with multiple selectors",
			fields: fields{
				VpcPrefixID: cutil.GetPtr(uuid.New().String()),
				SubnetID:    cutil.GetPtr(uuid.New().String()),
			},
			wantErr:          true,
			wantErrorMessage: "exactly one of `subnetId`, `vpcPrefixId`, or `vpcId` must be specified",
		},
		{
			name: "test invalid Interface request with VPC and VPC Prefix selectors",
			fields: fields{
				VpcID:       cutil.GetPtr(uuid.NewString()),
				VpcPrefixID: cutil.GetPtr(uuid.NewString()),
				IPFamilies:  []IPFamily{IPFamilyIPv4},
			},
			wantErr:          true,
			wantErrorMessage: "exactly one of `subnetId`, `vpcPrefixId`, or `vpcId` must be specified",
		},
		{
			name: "test invalid Interface request with VPC and Subnet selectors",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				SubnetID:   cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{IPFamilyIPv4},
			},
			wantErr:          true,
			wantErrorMessage: "exactly one of `subnetId`, `vpcPrefixId`, or `vpcId` must be specified",
		},
		{
			name:             "test invalid Interface request with no selector",
			fields:           fields{},
			wantErr:          true,
			wantErrorMessage: "exactly one of `subnetId`, `vpcPrefixId`, or `vpcId` must be specified",
		},
		{
			name: "test invalid Interface VPC request without IP families",
			fields: fields{
				VpcID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr:          true,
			wantErrorMessage: "must be specified when `vpcId` is specified",
		},
		{
			name: "test invalid Interface VPC request with empty IP families",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{},
			},
			wantErr:          true,
			wantErrorMessage: "must contain at least one value when `vpcId` is specified",
		},
		{
			name: "test invalid Interface request with IP families but no VPC",
			fields: fields{
				VpcPrefixID: cutil.GetPtr(uuid.NewString()),
				IPFamilies:  []IPFamily{IPFamilyIPv4},
			},
			wantErr:          true,
			wantErrorMessage: "cannot be specified without `vpcId`",
		},
		{
			name: "test invalid Interface VPC request with unknown IP family",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{"IPX"},
			},
			wantErr:          true,
			wantErrorMessage: "invalid IP family `IPX`",
		},
		{
			name: "test valid Interface IPv6-only VPC request",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{IPFamilyIPv6},
			},
			wantErr:        false,
			wantIPFamilies: []IPFamily{IPFamilyIPv6},
		},
		{
			name: "test valid Interface dual-stack VPC request",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{IPFamilyIPv4, IPFamilyIPv6},
			},
			wantErr:        false,
			wantIPFamilies: []IPFamily{IPFamilyIPv4, IPFamilyIPv6},
		},
		{
			name: "test valid Interface device and deviceInterface request",
			fields: fields{
				VpcPrefixID:    cutil.GetPtr(uuid.New().String()),
				IsPhysical:     true,
				Device:         cutil.GetPtr("test-device"),
				DeviceInstance: cutil.GetPtr(15),
			},
			wantErr: false,
		},
		{
			name: "test valid Interface VPC device and deviceInterface request",
			fields: fields{
				VpcID:          cutil.GetPtr(uuid.NewString()),
				IPFamilies:     []IPFamily{IPFamilyIPv4},
				IsPhysical:     true,
				Device:         cutil.GetPtr("test-device"),
				DeviceInstance: cutil.GetPtr(15),
			},
			wantErr: false,
		},
		{
			name: "test invalid Interface device and deviceInterface request",
			fields: fields{
				VpcPrefixID:    cutil.GetPtr(uuid.New().String()),
				IsPhysical:     false,
				Device:         cutil.GetPtr("test-device"),
				DeviceInstance: cutil.GetPtr(1),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface virtual function ID above range",
			fields: fields{
				VpcPrefixID:       cutil.GetPtr(uuid.New().String()),
				IPAddress:         cutil.GetPtr("192.0.2.11"),
				IsPhysical:        false,
				Device:            cutil.GetPtr("test-device"),
				DeviceInstance:    cutil.GetPtr(1),
				VirtualFunctionID: cutil.GetPtr(20),
			},
			wantErr: true,
		},
		{
			name: "test valid Interface virtual function ID lower bound",
			fields: fields{
				VpcPrefixID:       cutil.GetPtr(uuid.New().String()),
				IsPhysical:        false,
				Device:            cutil.GetPtr("test-device"),
				DeviceInstance:    cutil.GetPtr(1),
				VirtualFunctionID: cutil.GetPtr(0),
			},
			wantErr: false,
		},
		{
			name: "test valid Interface virtual function ID upper bound",
			fields: fields{
				VpcPrefixID:       cutil.GetPtr(uuid.New().String()),
				IsPhysical:        false,
				Device:            cutil.GetPtr("test-device"),
				DeviceInstance:    cutil.GetPtr(1),
				VirtualFunctionID: cutil.GetPtr(15),
			},
			wantErr: false,
		},
		{
			name: "test invalid Interface virtual function ID at old upper bound",
			fields: fields{
				VpcPrefixID:       cutil.GetPtr(uuid.New().String()),
				IsPhysical:        false,
				Device:            cutil.GetPtr("test-device"),
				DeviceInstance:    cutil.GetPtr(1),
				VirtualFunctionID: cutil.GetPtr(16),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface virtual function ID below range",
			fields: fields{
				VpcPrefixID:       cutil.GetPtr(uuid.New().String()),
				IsPhysical:        false,
				Device:            cutil.GetPtr("test-device"),
				DeviceInstance:    cutil.GetPtr(1),
				VirtualFunctionID: cutil.GetPtr(-1),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface device and deviceInstance request",
			fields: fields{
				Device:      cutil.GetPtr("test-device"),
				VpcPrefixID: cutil.GetPtr(uuid.New().String()),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface device and deviceInterface request",
			fields: fields{
				DeviceInstance: cutil.GetPtr(1),
				VpcPrefixID:    cutil.GetPtr(uuid.New().String()),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface ipAddress with subnet request",
			fields: fields{
				SubnetID:  cutil.GetPtr(uuid.New().String()),
				IPAddress: cutil.GetPtr("192.0.2.11"),
			},
			wantErr:          true,
			wantErrorMessage: "cannot be specified for Subnet based Interfaces",
		},
		{
			name: "test invalid Interface ipAddress without subnet or vpc prefix request",
			fields: fields{
				IPAddress: cutil.GetPtr("192.0.2.11"),
			},
			wantErr:          true,
			wantErrorMessage: "exactly one of `subnetId`, `vpcPrefixId`, or `vpcId` must be specified",
		},
		{
			name: "test invalid Interface ipAddress with VPC request",
			fields: fields{
				VpcID:      cutil.GetPtr(uuid.NewString()),
				IPFamilies: []IPFamily{IPFamilyIPv4},
				IPAddress:  cutil.GetPtr("192.0.2.11"),
			},
			wantErr:          true,
			wantErrorMessage: "cannot be specified when `vpcId` is specified",
		},
		{
			name: "test invalid Interface ipAddress with final host bit 0",
			fields: fields{
				VpcPrefixID: cutil.GetPtr(uuid.New().String()),
				IPAddress:   cutil.GetPtr("192.0.2.10"),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface ipAddress request",
			fields: fields{
				VpcPrefixID: cutil.GetPtr(uuid.New().String()),
				IPAddress:   cutil.GetPtr("not-an-ip"),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface device and deviceInterface request",
			fields: fields{
				Device:         cutil.GetPtr("test-device"),
				DeviceInstance: cutil.GetPtr(1),
			},
			wantErr: true,
		},
		{
			name: "test invalid Interface device and deviceInterface request",
			fields: fields{
				Device:         cutil.GetPtr("test-device"),
				DeviceInstance: cutil.GetPtr(1),
				SubnetID:       cutil.GetPtr(uuid.New().String()),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			iscr := APIInterfaceCreateOrUpdateRequest{
				SubnetID:          tt.fields.SubnetID,
				VpcPrefixID:       tt.fields.VpcPrefixID,
				VpcID:             tt.fields.VpcID,
				IPFamilies:        tt.fields.IPFamilies,
				IPAddress:         tt.fields.IPAddress,
				IsPhysical:        tt.fields.IsPhysical,
				Device:            tt.fields.Device,
				DeviceInstance:    tt.fields.DeviceInstance,
				VirtualFunctionID: tt.fields.VirtualFunctionID,
			}
			err := iscr.Validate()
			if (err != nil) != tt.wantErr {
				t.Errorf("APIInterfaceCreateOrUpdateRequest.Validate() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.wantErrorMessage != "" && err != nil {
				assert.Contains(t, err.Error(), tt.wantErrorMessage)
			}

			if tt.wantIPFamilies != nil {
				assert.Equal(t, tt.wantIPFamilies, iscr.IPFamilies)
			}
		})
	}
}

func TestAPIInterfaceCreateOrUpdateRequest_VpcIPFamilyMode(t *testing.T) {
	tests := []struct {
		name       string
		families   []IPFamily
		wantFamily cdbm.InterfaceVpcIPFamilyMode
	}{
		{
			name:       "IPv4-only",
			families:   []IPFamily{IPFamilyIPv4},
			wantFamily: cdbm.InterfaceVpcIPFamilyModeIPv4Only,
		},
		{
			name:       "IPv6-only",
			families:   []IPFamily{IPFamilyIPv6},
			wantFamily: cdbm.InterfaceVpcIPFamilyModeIPv6Only,
		},
		{
			name:       "dual-stack",
			families:   []IPFamily{IPFamilyIPv4, IPFamilyIPv6},
			wantFamily: cdbm.InterfaceVpcIPFamilyModeDualStack,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := APIInterfaceCreateOrUpdateRequest{IPFamilies: tt.families}
			assert.Equal(t, tt.wantFamily, req.VpcIPFamilyMode())
		})
	}
}
