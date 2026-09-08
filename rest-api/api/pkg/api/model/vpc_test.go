// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"math"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIVpcCreateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                      string
		Description               *string
		SiteID                    string
		NetworkVirtualizationType *string
		Labels                    map[string]string
		Vni                       *int
		RoutingProfile            *string
		RoutingProfileOverrides   *APIVpcRoutingProfileOverrides
	}
	tests := []struct {
		name            string
		fields          fields
		wantErr         bool
		wantErrContains string
	}{
		{
			name: "test valid VPC create request",
			fields: fields{
				Name:        "test-name",
				Description: cutil.GetPtr("Test description"),
				SiteID:      uuid.NewString(),
			},
			wantErr: false,
		},
		{
			name: "test valid VPC create request - invalid names are specified names exceeded 256 char",
			fields: fields{
				Name:        "apvhhigcgctlgiwtbrgldkegmnwuqcibutndlholygxvhzrpinziepszvpmopvzkybykrwgvzojtssorabkrnawgjzeuuerphsnecipubeuzrpewkfuvwoeybagaxpvjvzvbzqznyfmcpbxrhbdkhewiepykfjeejeqatswgrlhqkgnvwqmatejufnsjgelcugcoccybywdrnlyvsegsegorygwdvurgktpuzyrsoutspsnyzynliaxwseazqmimp",
				Description: cutil.GetPtr("Test description"),
				SiteID:      uuid.NewString(),
			},
			wantErr: true,
		},
		{
			name: "test invalid VPC create request - invalid Site ID",
			fields: fields{
				Name:   "test-name",
				SiteID: "invalid-uuid",
			},
			wantErr: true,
		},
		{
			name: "test invalid VPC create request - invalid Network Virtualization Type",
			fields: fields{
				Name:                      "test-name",
				Description:               cutil.GetPtr("Test description"),
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr("VPC"),
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - valid labels are specified",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.NewString(),
				Labels: map[string]string{
					"name":        "a-nv100-VPC",
					"description": "",
				},
			},
			wantErr: false,
		},
		{
			name: "test valid VPC create request - routing profile for FNN",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				RoutingProfile:            cutil.GetPtr(APIVpcRoutingProfileInternal),
			},
			wantErr: false,
		},
		// Inline overrides are accepted when the request explicitly selects FNN.
		{
			name: "test valid VPC create request - routing profile overrides for FNN",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				RoutingProfileOverrides:   &APIVpcRoutingProfileOverrides{LeakDefaultRouteFromUnderlay: cutil.GetPtr(true)},
			},
			wantErr: false,
		},
		// Explicit non-FNN requests identify the rejected virtualization type.
		{
			name: "test invalid VPC create request - routing profile overrides on non-FNN VPC",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
				RoutingProfileOverrides:   &APIVpcRoutingProfileOverrides{LeakDefaultRouteFromUnderlay: cutil.GetPtr(true)},
			},
			wantErr:         true,
			wantErrContains: "`networkVirtualizationType` is `ETHERNET_VIRTUALIZER`",
		},
		{
			name: "test invalid VPC create request - routing profile on non-FNN VPC",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
				RoutingProfile:            cutil.GetPtr(APIVpcRoutingProfileInternal),
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - Flat virtualization type",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFlat),
			},
			wantErr: false,
		},
		{
			name: "test invalid VPC create request - routing profile on Flat VPC",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFlat),
				RoutingProfile:            cutil.GetPtr(APIVpcRoutingProfileInternal),
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - routing profile when network virtualization type is omitted",
			fields: fields{
				Name:           "test-name",
				SiteID:         uuid.NewString(),
				RoutingProfile: cutil.GetPtr(APIVpcRoutingProfileExternal),
			},
			wantErr: false,
		},
		{
			name: "test invalid VPC create request - routing profile too short",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				RoutingProfile:            cutil.GetPtr("ab"),
			},
			wantErr: true,
		},
		{
			name: "test invalid VPC create request - routing profile starts with non-letter",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				RoutingProfile:            cutil.GetPtr("1internal"),
			},
			wantErr: true,
		},
		{
			name: "test invalid VPC create request - routing profile contains underscore",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				RoutingProfile:            cutil.GetPtr("privileged_internal"),
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - site-configured routing profile",
			fields: fields{
				Name:                      "test-name",
				SiteID:                    uuid.NewString(),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				RoutingProfile:            cutil.GetPtr("tenant-edge"),
			},
			wantErr: false,
		},
		{
			name: "test invalid VPC create request - invalid VNI",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.NewString(),
				Vni:    cutil.GetPtr(70000),
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - invalid labels are specified key is empty",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.NewString(),
				Labels: map[string]string{
					"name": "a-nv200=VPC",
					"":     "test",
				},
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - invalid labels are specified both key and value are empty",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.NewString(),
				Labels: map[string]string{
					"name": "a-nv300=VPC",
					"":     "",
				},
			},
			wantErr: true,
		},
		{
			name: "test valid VPC create request - invalid labels are specified key has char more than 256",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.NewString(),
				Labels: map[string]string{
					"ygsV9MoUjep1rCwbQskkF9wfMolE3oDTCcxuYSJCx9TLKepCIku9pnHfIkxCxHkb7ucbsBL4hyLqQaHoEqpTBmfoX4Un7sGvQdHGZ7nb68JJEJ3ocFAtyCMCBt66z3ldnTqp8SXXOIhNsOh35MLYQjI8557Pu6o91TsEBqyTz0yz68HHmfNgJoreHpXfeujq4cpElUXXbQ3xfFICkNyghXgFZ0MLs2o0u1Nd29aB113X5g3FKJBCskW6eBULNmeFFG61DMM37q": "a-nv300=VPC",
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vcr := APIVpcCreateRequest{
				Name:                      tt.fields.Name,
				Description:               tt.fields.Description,
				SiteID:                    tt.fields.SiteID,
				NetworkVirtualizationType: tt.fields.NetworkVirtualizationType,
				Labels:                    tt.fields.Labels,
				Vni:                       tt.fields.Vni,
				RoutingProfile:            tt.fields.RoutingProfile,
				RoutingProfileOverrides:   tt.fields.RoutingProfileOverrides,
			}

			err := vcr.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIVpcCreateRequest.Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
			if tt.wantErrContains != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tt.wantErrContains)
			}
		})
	}
}

func TestAPIVpcUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                    string
		Description             *string
		Labels                  map[string]string
		RoutingProfileOverrides *APIVpcRoutingProfileOverrides
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test valid VPC update request",
			fields: fields{
				Name:        "test-name",
				Description: cutil.GetPtr("Test description"),
			},
			wantErr: false,
		},
		// Update validates the inline definition while the handler checks the persisted VPC type.
		{
			name: "test valid VPC update request - routing profile overrides",
			fields: fields{
				Name:                    "test-name",
				RoutingProfileOverrides: &APIVpcRoutingProfileOverrides{LeakDefaultRouteFromUnderlay: cutil.GetPtr(true)},
			},
			wantErr: false,
		},
		{
			name: "test valid VPC update request - invalid names are specified names exceeded 256 char",
			fields: fields{
				Name:        "apvhhigcgctlgiwtbrgldkegmnwuqcibutndlholygxvhzrpinziepszvpmopvzkybykrwgvzojtssorabkrnawgjzeuuerphsnecipubeuzrpewkfuvwoeybagaxpvjvzvbzqznyfmcpbxrhbdkhewiepykfjeejeqatswgrlhqkgnvwqmatejufnsjgelcugcoccybywdrnlyvsegsegorygwdvurgktpuzyrsoutspsnyzynliaxwseazqmimp",
				Description: cutil.GetPtr("Test description"),
			},
			wantErr: true,
		},
		{
			name: "test valid VPC update request - valid labels are specified",
			fields: fields{
				Name: "test-name",
				Labels: map[string]string{
					"name":        "a-nv100-VPC",
					"description": "",
				},
			},
			wantErr: false,
		},
		{
			name: "test valid VPC update request - invalid labels are specified key is empty",
			fields: fields{
				Name: "test-name",
				Labels: map[string]string{
					"name": "a-nv200=VPC",
					"":     "test",
				},
			},
			wantErr: true,
		},
		{
			name: "test valid VPC update request - invalid labels are specified both key and value are empty",
			fields: fields{
				Name: "test-name",
				Labels: map[string]string{
					"name": "a-nv300=VPC",
					"":     "",
				},
			},
			wantErr: true,
		},
		{
			name: "test valid VPC update request - invalid labels are specified key has char more than 128",
			fields: fields{
				Name: "test-name",
				Labels: map[string]string{
					"morethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128charmorethan128char": "a-nv300=VPC",
					"": "",
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vur := APIVpcUpdateRequest{
				Name:                    &tt.fields.Name,
				Description:             tt.fields.Description,
				Labels:                  tt.fields.Labels,
				RoutingProfileOverrides: tt.fields.RoutingProfileOverrides,
			}

			if err := vur.Validate(); (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIVpcUpdateRequest.Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
		})
	}
}

// TestAPIVpcRoutingProfileOverrides_Validate verifies the API rejects values
// that cannot be represented by Core while preserving valid presence semantics.
func TestAPIVpcRoutingProfileOverrides_Validate(t *testing.T) {
	tests := []struct {
		name    string
		profile *APIVpcRoutingProfileOverrides
		wantErr bool
	}{
		// Empty lists, duplicate prefixes, host bits, and both IP families are valid Core inputs.
		{
			name: "accepts Core-compatible values",
			profile: &APIVpcRoutingProfileOverrides{
				RouteTargetImports: &APIVpcRouteTargets{
					{ASN: 0, VNI: 0},
					{ASN: math.MaxUint32, VNI: math.MaxUint32},
				},
				RouteTargetsOnExports:        &APIVpcRouteTargets{},
				AcceptedLeaksFromUnderlay:    &[]string{"10.0.0.1/24", "10.0.0.1/24", "2001:db8::1/64"},
				AllowedAnycastPrefixes:       &[]string{},
				LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
			},
		},
		// Malformed prefixes must not reach Core's IpNetwork parser.
		{
			name: "rejects malformed prefixes",
			profile: &APIVpcRoutingProfileOverrides{
				AllowedAnycastPrefixes: &[]string{"not-a-prefix"},
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.profile.Validate()
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

// TestAPIVpcRoutingProfileOverrides_ToDB verifies optional values retain their
// presence and explicit-empty semantics in the persisted representation.
func TestAPIVpcRoutingProfileOverrides_ToDB(t *testing.T) {
	imports := APIVpcRouteTargets{{ASN: 64512, VNI: 17}}
	emptyTargets := APIVpcRouteTargets{}
	emptyPrefixes := []string{}
	profile := &APIVpcRoutingProfileOverrides{
		RouteTargetImports:            &imports,
		RouteTargetsOnExports:         &emptyTargets,
		LeakDefaultRouteFromUnderlay:  cutil.GetPtr(false),
		TenantLeakCommunitiesAccepted: cutil.GetPtr(true),
		AcceptedLeaksFromUnderlay:     &emptyPrefixes,
		AllowedAnycastPrefixes:        &[]string{"192.0.2.1/24"},
	}

	dbProfile := profile.ToDB()
	require.NotNil(t, dbProfile)
	require.NotNil(t, dbProfile.RouteTargetImports)
	assert.Equal(t, []cdbm.VpcRouteTarget{{ASN: 64512, VNI: 17}}, *dbProfile.RouteTargetImports)
	require.NotNil(t, dbProfile.RouteTargetsOnExports)
	assert.Empty(t, *dbProfile.RouteTargetsOnExports)
	require.NotNil(t, dbProfile.LeakDefaultRouteFromUnderlay)
	assert.False(t, *dbProfile.LeakDefaultRouteFromUnderlay)
	require.NotNil(t, dbProfile.AcceptedLeaksFromUnderlay)
	assert.Empty(t, *dbProfile.AcceptedLeaksFromUnderlay)
}

// TestAPIVpcRoutingProfileOverrides_FromDB verifies persisted optional values
// retain their presence and explicit-empty semantics in the API representation.
func TestAPIVpcRoutingProfileOverrides_FromDB(t *testing.T) {
	emptyTargets := []cdbm.VpcRouteTarget{}
	emptyPrefixes := []string{}
	dbProfile := &cdbm.VpcRoutingProfileOverrides{
		RouteTargetImports:            &[]cdbm.VpcRouteTarget{{ASN: 64512, VNI: 17}},
		RouteTargetsOnExports:         &emptyTargets,
		LeakDefaultRouteFromUnderlay:  cutil.GetPtr(false),
		TenantLeakCommunitiesAccepted: cutil.GetPtr(true),
		AcceptedLeaksFromUnderlay:     &emptyPrefixes,
		AllowedAnycastPrefixes:        &[]string{"192.0.2.1/24"},
	}

	profile := &APIVpcRoutingProfileOverrides{}
	profile.FromDB(dbProfile)
	require.NotNil(t, profile.RouteTargetImports)
	assert.Equal(t, APIVpcRouteTargets{{ASN: 64512, VNI: 17}}, *profile.RouteTargetImports)
	require.NotNil(t, profile.RouteTargetsOnExports)
	assert.Empty(t, *profile.RouteTargetsOnExports)
	require.NotNil(t, profile.LeakDefaultRouteFromUnderlay)
	assert.False(t, *profile.LeakDefaultRouteFromUnderlay)
	require.NotNil(t, profile.AcceptedLeaksFromUnderlay)
	assert.Empty(t, *profile.AcceptedLeaksFromUnderlay)
}

func TestAPIVpcVirtualizationUpdateRequest_Validate(t *testing.T) {
	vpcObj1 := &cdbm.Vpc{
		ID:                        uuid.New(),
		Name:                      "test",
		Org:                       "test",
		SiteID:                    uuid.New(),
		TenantID:                  uuid.New(),
		InfrastructureProviderID:  uuid.New(),
		NetworkVirtualizationType: cutil.GetPtr("ETHERNET_VIRTUALIZER"),
		Created:                   cdb.GetCurTime(),
		Updated:                   cdb.GetCurTime(),
	}

	vpcObj2 := &cdbm.Vpc{
		ID:                        uuid.New(),
		Name:                      "test1",
		Org:                       "test1",
		SiteID:                    uuid.New(),
		TenantID:                  uuid.New(),
		InfrastructureProviderID:  uuid.New(),
		NetworkVirtualizationType: cutil.GetPtr("FNN"),
		Created:                   cdb.GetCurTime(),
		Updated:                   cdb.GetCurTime(),
	}

	type fields struct {
		NetworkVirtualizationType string
		inputVpc                  *cdbm.Vpc
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test valid VPC virtualization update request",
			fields: fields{
				NetworkVirtualizationType: "FNN",
				inputVpc:                  vpcObj1,
			},
			wantErr: false,
		},
		{
			name: "test invalid VPC virtualization update request - support only FNN",
			fields: fields{
				NetworkVirtualizationType: "ETHERNET_VIRTUALIZER",
				inputVpc:                  vpcObj1,
			},
			wantErr: true,
		},
		{
			name: "test invalid VPC virtualization update request - existing vpc already FNN",
			fields: fields{
				NetworkVirtualizationType: "FNN",
				inputVpc:                  vpcObj2,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vvur := APIVpcVirtualizationUpdateRequest{
				NetworkVirtualizationType: tt.fields.NetworkVirtualizationType,
			}

			if err := vvur.Validate(tt.fields.inputVpc); (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIVpcVirtualizationUpdateRequest.Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
		})
	}
}

func TestNewAPIVpc(t *testing.T) {
	type args struct {
		dbVpc cdbm.Vpc
		dbsds []cdbm.StatusDetail
	}

	dbVpc := cdbm.Vpc{
		ID:                        uuid.New(),
		Name:                      "test-vpc",
		Description:               cutil.GetPtr("Test VPC Description"),
		Org:                       "test-org",
		TenantID:                  uuid.New(),
		SiteID:                    uuid.New(),
		NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
		SlaacEnabled:              true,
		RoutingProfile:            cutil.GetPtr(apiVpcRoutingProfileSiteInternal),
		ControllerVpcID:           cutil.GetPtr(uuid.New()),
		// The normal expectation is that Vni and ActiveVni match or
		// that Vni is simply null, but we want to test for correctness
		// in the conversion from the record in the DB and the API struct.
		Vni:       cutil.GetPtr(555),
		ActiveVni: cutil.GetPtr(777),
		Labels: map[string]string{
			"zone": "1",
			"west": "2",
		},
		Status:  cdbm.SiteStatusPending,
		Created: time.Now(),
		Updated: time.Now(),
	}

	dbsds := []cdbm.StatusDetail{
		{
			ID:      uuid.New(),
			Status:  cdbm.SiteStatusPending,
			Created: time.Now(),
			Updated: time.Now(),
		},
	}

	apidbsh := []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apidbsh = append(apidbsh, NewAPIStatusDetail(dbsd))
	}

	tests := []struct {
		name string
		args args
		want APIVpc
	}{
		{
			name: "get new APIVpc returns stored routing profile",
			args: args{
				dbVpc: dbVpc,
				dbsds: dbsds,
			},
			want: APIVpc{
				ID:                        dbVpc.ID.String(),
				Name:                      dbVpc.Name,
				Description:               dbVpc.Description,
				Org:                       dbVpc.Org,
				InfrastructureProviderID:  util.GetUUIDPtrToStrPtr(&dbVpc.InfrastructureProviderID),
				TenantID:                  util.GetUUIDPtrToStrPtr(&dbVpc.TenantID),
				SiteID:                    util.GetUUIDPtrToStrPtr(&dbVpc.SiteID),
				NetworkVirtualizationType: dbVpc.NetworkVirtualizationType,
				SlaacEnabled:              true,
				RoutingProfile:            cutil.GetPtr(APIVpcRoutingProfileInternal),
				ControllerVpcID:           util.GetUUIDPtrToStrPtr(dbVpc.ControllerVpcID),
				RequestedVni:              dbVpc.Vni,
				Vni:                       dbVpc.ActiveVni,
				Status:                    dbVpc.Status,
				Labels: map[string]string{
					"zone": "1",
					"west": "2",
				},
				StatusHistory: apidbsh,
				Created:       dbVpc.Created,
				Updated:       dbVpc.Updated,
			},
		},
		{
			name: "get new APIVpc includes routing profile for FNN VPC",
			args: args{
				dbVpc: func() cdbm.Vpc {
					fnnVpc := dbVpc
					fnnVpc.NetworkVirtualizationType = cutil.GetPtr(cdbm.VpcFNN)
					return fnnVpc
				}(),
				dbsds: dbsds,
			},
			want: APIVpc{
				ID:                        dbVpc.ID.String(),
				Name:                      dbVpc.Name,
				Description:               dbVpc.Description,
				Org:                       dbVpc.Org,
				InfrastructureProviderID:  util.GetUUIDPtrToStrPtr(&dbVpc.InfrastructureProviderID),
				TenantID:                  util.GetUUIDPtrToStrPtr(&dbVpc.TenantID),
				SiteID:                    util.GetUUIDPtrToStrPtr(&dbVpc.SiteID),
				NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				SlaacEnabled:              true,
				RoutingProfile:            cutil.GetPtr(APIVpcRoutingProfileInternal),
				ControllerVpcID:           util.GetUUIDPtrToStrPtr(dbVpc.ControllerVpcID),
				RequestedVni:              dbVpc.Vni,
				Vni:                       dbVpc.ActiveVni,
				Status:                    dbVpc.Status,
				Labels: map[string]string{
					"zone": "1",
					"west": "2",
				},
				StatusHistory: apidbsh,
				Created:       dbVpc.Created,
				Updated:       dbVpc.Updated,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIVpc(tt.args.dbVpc, tt.args.dbsds, false)

			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, tt.want.Description, got.Description)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, *tt.want.InfrastructureProviderID, *got.InfrastructureProviderID)
			assert.Equal(t, *tt.want.TenantID, *got.TenantID)
			assert.Equal(t, *tt.want.SiteID, *got.SiteID)
			assert.Equal(t, tt.want.NetworkVirtualizationType, got.NetworkVirtualizationType)
			assert.Equal(t, tt.want.SlaacEnabled, got.SlaacEnabled)
			assert.Equal(t, tt.want.RoutingProfile, got.RoutingProfile)
			assert.Equal(t, *tt.want.ControllerVpcID, *got.ControllerVpcID)
			if tt.want.Vni != nil {
				assert.NotNil(t, got.Vni)
				assert.Equal(t, *tt.want.Vni, *got.Vni)
			}
			if tt.want.RequestedVni != nil {
				assert.NotNil(t, got.RequestedVni)
				assert.Equal(t, *tt.want.RequestedVni, *got.RequestedVni)
			}
			assert.Equal(t, len(tt.want.Labels), len(got.Labels))
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.StatusHistory, got.StatusHistory)
			assert.Equal(t, tt.want.Created, got.Created)
			assert.Equal(t, tt.want.Updated, got.Updated)
		})
	}

	profileVpc := cdbm.Vpc{
		ID: uuid.New(),
		RoutingProfileOverrides: &cdbm.VpcRoutingProfileOverrides{
			LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
		},
		EffectiveRoutingProfile: &cdbm.VpcEffectiveRoutingProfile{
			LeakDefaultRouteFromUnderlay: true,
			Internal:                     true,
			AccessTier:                   7,
		},
	}

	t.Run("omits effective profile without targeted instance creation permission", func(t *testing.T) {
		// Desired configuration remains visible even when the resolved state is gated.
		unprivileged := NewAPIVpc(profileVpc, nil, false)
		require.NotNil(t, unprivileged.RoutingProfileOverrides)
		assert.Nil(t, unprivileged.EffectiveRoutingProfile)
		unprivilegedJSON, err := json.Marshal(unprivileged)
		require.NoError(t, err)
		assert.NotContains(t, string(unprivilegedJSON), "effectiveRoutingProfile")
	})

	t.Run("includes effective profile with targeted instance creation permission", func(t *testing.T) {
		// Authorized responses expose resolved values and canonical empty lists.
		privileged := NewAPIVpc(profileVpc, nil, true)
		require.NotNil(t, privileged.EffectiveRoutingProfile)
		assert.True(t, privileged.EffectiveRoutingProfile.Internal)
		assert.Equal(t, uint32(7), privileged.EffectiveRoutingProfile.AccessTier)
		assert.NotNil(t, privileged.EffectiveRoutingProfile.AcceptedLeaksFromUnderlay)
		assert.Empty(t, privileged.EffectiveRoutingProfile.AcceptedLeaksFromUnderlay)
		assert.NotNil(t, privileged.EffectiveRoutingProfile.AllowedAnycastPrefixes)
		assert.Empty(t, privileged.EffectiveRoutingProfile.AllowedAnycastPrefixes)
	})
}

func TestAPIVpcCreateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"
	nsg := "nsg-1"
	nvllpID := uuid.New()
	fnn := cdbm.VpcFNN
	eth := cdbm.VpcEthernetVirtualizer

	t.Run("populates id, metadata, NSG, NVLink, and create-specific fields", func(t *testing.T) {
		profile := apiVpcRoutingProfileSiteInternal
		vpc := &cdbm.Vpc{
			ID:                        id,
			Org:                       "org-1",
			Name:                      "vpc-a",
			Description:               &desc,
			NetworkSecurityGroupID:    &nsg,
			NVLinkLogicalPartitionID:  &nvllpID,
			NetworkVirtualizationType: &fnn,
			RoutingProfile:            &profile,
			Labels:                    map[string]string{"env": "prod"},
		}
		vni := 4242
		slaacEnabled := true
		got := APIVpcCreateRequest{
			Vni:            &vni,
			SlaacEnabled:   &slaacEnabled,
			RoutingProfile: cutil.GetPtr(APIVpcRoutingProfileInternal),
		}.ToProto(vpc)

		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		assert.Equal(t, "vpc-a", got.Name)
		assert.Equal(t, "org-1", got.TenantOrganizationId)
		require.NotNil(t, got.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_FNN, *got.NetworkVirtualizationType)
		require.NotNil(t, got.RoutingProfileType)
		assert.Equal(t, apiVpcRoutingProfileSiteInternal, *got.RoutingProfileType)
		require.NotNil(t, got.NetworkSecurityGroupId)
		assert.Equal(t, "nsg-1", *got.NetworkSecurityGroupId)
		require.NotNil(t, got.Vni)
		assert.Equal(t, uint32(4242), *got.Vni)
		require.NotNil(t, got.SlaacEnabled)
		assert.True(t, *got.SlaacEnabled)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "vpc-a", got.Metadata.Name)
		assert.Equal(t, "primary", got.Metadata.Description)
		require.Len(t, got.Metadata.Labels, 1)
		assert.Equal(t, "env", got.Metadata.Labels[0].GetKey())
		assert.Equal(t, "prod", got.Metadata.Labels[0].GetValue())
		require.NotNil(t, got.DefaultNvlinkLogicalPartitionId)
		assert.Equal(t, nvllpID.String(), got.DefaultNvlinkLogicalPartitionId.Value)
	})

	t.Run("derives ETHERNET_VIRTUALIZER from the entity's DB column", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Org: "org-1", Name: "vpc-a", NetworkVirtualizationType: &eth}
		got := APIVpcCreateRequest{}.ToProto(vpc)
		require.NotNil(t, got.NetworkVirtualizationType)
		assert.Equal(t, corev1.VpcVirtualizationType_ETHERNET_VIRTUALIZER, *got.NetworkVirtualizationType)
	})

	t.Run("omits NetworkVirtualizationType when the entity has none", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Org: "org-1", Name: "vpc-a"}
		got := APIVpcCreateRequest{}.ToProto(vpc)
		assert.Nil(t, got.NetworkVirtualizationType)
		assert.Nil(t, got.RoutingProfileType)
		assert.Nil(t, got.Vni)
		assert.Nil(t, got.SlaacEnabled)
		assert.Nil(t, got.NetworkSecurityGroupId)
		assert.Nil(t, got.DefaultNvlinkLogicalPartitionId)
	})

	t.Run("nil request RoutingProfile leaves the wire field unset even if the entity carries one", func(t *testing.T) {
		// The wire follows the API request shape: if the caller did
		// not ask for a routingProfile, we don't echo a stale entity
		// value into the create request.
		profile := apiVpcRoutingProfileSiteInternal
		vpc := &cdbm.Vpc{ID: id, Org: "org-1", Name: "vpc-a", NetworkVirtualizationType: &fnn, RoutingProfile: &profile}
		got := APIVpcCreateRequest{}.ToProto(vpc)
		assert.Nil(t, got.RoutingProfileType)
	})

	t.Run("forwards supplied routing profile overrides", func(t *testing.T) {
		// Create forwards the complete inline definition supplied by the caller.
		vpc := &cdbm.Vpc{ID: id, Org: "org-1", Name: "vpc-a", NetworkVirtualizationType: &fnn}
		profile := &APIVpcRoutingProfileOverrides{
			LeakTenantHostRoutesToUnderlay: cutil.GetPtr(false),
			AllowedAnycastPrefixes:         &[]string{},
		}
		got := (APIVpcCreateRequest{RoutingProfileOverrides: profile}).ToProto(vpc)
		require.NotNil(t, got.RoutingProfileOverrides)
		require.NotNil(t, got.RoutingProfileOverrides.LeakTenantHostRoutesToUnderlay)
		assert.False(t, *got.RoutingProfileOverrides.LeakTenantHostRoutesToUnderlay)
		require.NotNil(t, got.RoutingProfileOverrides.AllowedAnycastPrefixes)
	})

	t.Run("forwards persisted power resource group", func(t *testing.T) {
		persistedPowerResourceGroup := "power-rg-persisted"
		requestedPowerResourceGroup := "power-rg-requested"
		vpc := &cdbm.Vpc{ID: id, Org: "org-1", Name: "vpc-a", PowerResourceGroup: &persistedPowerResourceGroup}
		got := (APIVpcCreateRequest{PowerResourceGroup: &requestedPowerResourceGroup}).ToProto(vpc)
		require.NotNil(t, got.PowerResourceGroup)
		assert.Equal(t, persistedPowerResourceGroup, *got.PowerResourceGroup)
	})
}

func TestVpcResponseIncludesDisabledSlaacField(t *testing.T) {
	vpc := cdbm.Vpc{
		ID:           uuid.New(),
		Name:         "non-slaac-vpc",
		SlaacEnabled: false,
		Status:       cdbm.VpcStatusReady,
	}

	data, err := json.Marshal(NewAPIVpc(vpc, nil, false))
	require.NoError(t, err)

	var payload map[string]any
	require.NoError(t, json.Unmarshal(data, &payload))
	value, present := payload["slaacEnabled"]
	require.True(t, present)
	assert.Equal(t, false, value)
}

func TestAPIVpcUpdateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"
	nsg := "nsg-1"
	other := "nsg-other"
	empty := ""
	nvllpID := uuid.New()

	t.Run("populates id, metadata, and NSG from the merged-into-DB vpc", func(t *testing.T) {
		vpc := &cdbm.Vpc{
			ID:                     id,
			Name:                   "vpc-a",
			Description:            &desc,
			NetworkSecurityGroupID: &nsg,
			Labels:                 map[string]string{"env": "prod"},
		}
		got := APIVpcUpdateRequest{}.ToProto(vpc)
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.NetworkSecurityGroupId)
		assert.Equal(t, "nsg-1", *got.NetworkSecurityGroupId)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "vpc-a", got.Metadata.Name)
		assert.Equal(t, "primary", got.Metadata.Description)
		require.Len(t, got.Metadata.Labels, 1)
		assert.Nil(t, got.DefaultNvlinkLogicalPartitionId)
	})

	t.Run("falls back to entity NSG when request did not touch the field", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a", NetworkSecurityGroupID: &nsg}
		got := APIVpcUpdateRequest{}.ToProto(vpc)
		require.NotNil(t, got.NetworkSecurityGroupId)
		assert.Equal(t, "nsg-1", *got.NetworkSecurityGroupId)
	})

	t.Run("serializes cleared NSG as omitted field", func(t *testing.T) {
		// Simulates the handler path: handler cleared the DB row, so
		// vpc.NetworkSecurityGroupID is now nil. Core interprets an
		// omitted field as clear; a present empty string is invalid.
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a", NetworkSecurityGroupID: nil}
		got := APIVpcUpdateRequest{NetworkSecurityGroupID: &empty}.ToProto(vpc)
		assert.Nil(t, got.NetworkSecurityGroupId)
	})

	t.Run("uses entity NSG rather than raw API request value", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a", NetworkSecurityGroupID: &nsg}
		got := APIVpcUpdateRequest{NetworkSecurityGroupID: &other}.ToProto(vpc)
		require.NotNil(t, got.NetworkSecurityGroupId)
		assert.Equal(t, "nsg-1", *got.NetworkSecurityGroupId)
	})

	t.Run("serializes cleared NVLink default partition as omitted field", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a", NVLinkLogicalPartitionID: nil}
		got := APIVpcUpdateRequest{NVLinkLogicalPartitionID: &empty}.ToProto(vpc)
		assert.Nil(t, got.DefaultNvlinkLogicalPartitionId)
	})

	t.Run("NVLink override sends the entity-resolved partition ID", func(t *testing.T) {
		nvllpStr := nvllpID.String()
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a", NVLinkLogicalPartitionID: &nvllpID}
		got := APIVpcUpdateRequest{NVLinkLogicalPartitionID: &nvllpStr}.ToProto(vpc)
		require.NotNil(t, got.DefaultNvlinkLogicalPartitionId)
		assert.Equal(t, nvllpID.String(), got.DefaultNvlinkLogicalPartitionId.Value)
	})

	t.Run("omits NVLink when neither request nor entity carry it", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a"}
		got := APIVpcUpdateRequest{}.ToProto(vpc)
		assert.Nil(t, got.DefaultNvlinkLogicalPartitionId)
	})

	t.Run("uses ControllerVpcID for the request Id when set", func(t *testing.T) {
		ctrlID := uuid.New()
		vpc := &cdbm.Vpc{ID: id, ControllerVpcID: &ctrlID, Name: "vpc-a"}
		got := APIVpcUpdateRequest{}.ToProto(vpc)
		require.NotNil(t, got.Id)
		assert.Equal(t, ctrlID.String(), got.Id.Value)
	})

	t.Run("preserves omitted routing profile overrides", func(t *testing.T) {
		// Omitted input must not replace the current Core definition.
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a"}
		got := (APIVpcUpdateRequest{}).ToProto(vpc)
		assert.Nil(t, got.RoutingProfileOverrides)
	})

	t.Run("forwards explicit empty routing profile overrides", func(t *testing.T) {
		// An empty object restores inheritance for every property.
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a"}
		got := (APIVpcUpdateRequest{RoutingProfileOverrides: &APIVpcRoutingProfileOverrides{}}).ToProto(vpc)
		require.NotNil(t, got.RoutingProfileOverrides)
	})

	t.Run("preserves power resource group update presence", func(t *testing.T) {
		vpc := &cdbm.Vpc{ID: id, Name: "vpc-a"}
		set := "power-rg-a"

		got := (APIVpcUpdateRequest{}).ToProto(vpc)
		assert.Nil(t, got.PowerResourceGroup)

		got = (APIVpcUpdateRequest{PowerResourceGroup: &set}).ToProto(vpc)
		require.NotNil(t, got.PowerResourceGroup)
		assert.Equal(t, set, *got.PowerResourceGroup)

		got = (APIVpcUpdateRequest{PowerResourceGroup: &empty}).ToProto(vpc)
		require.NotNil(t, got.PowerResourceGroup)
		assert.Equal(t, "", *got.PowerResourceGroup)
	})
}

func TestAPIVpcUpdateRequest_PowerResourceGroupJSON(t *testing.T) {
	tests := []struct {
		name    string
		payload string
		want    *string
	}{
		{name: "omitted", payload: `{}`},
		{name: "null", payload: `{"powerResourceGroup":null}`},
		{name: "clear", payload: `{"powerResourceGroup":""}`, want: cutil.GetPtr("")},
		{name: "set", payload: `{"powerResourceGroup":"power-rg-a"}`, want: cutil.GetPtr("power-rg-a")},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var request APIVpcUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(tt.payload), &request))
			assert.Equal(t, tt.want, request.PowerResourceGroup)
		})
	}
}
