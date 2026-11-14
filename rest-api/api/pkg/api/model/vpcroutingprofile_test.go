// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
)

const routingVpcID = "a8545cb9-97b8-475a-befb-94cab9c96606"

func TestAPIVpcRoutingProfileUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		profile string
		vni     *int
		wantErr bool
	}{
		{name: "automatic custom profile", profile: "tenant_external_2"},
		{name: "short custom profile", profile: "x"},
		{name: "maximum profile length counts characters", profile: strings.Repeat("é", 64)},
		{name: "profile exceeds persistence limit", profile: strings.Repeat("é", 65), wantErr: true},
		{name: "minimum exact VNI", profile: "external", vni: cutil.GetPtr(1)},
		{name: "maximum exact VNI", profile: "external", vni: cutil.GetPtr(maxVpcRoutingVni)},
		{name: "missing profile", wantErr: true},
		{name: "zero VNI", profile: "external", vni: cutil.GetPtr(0), wantErr: true},
		{name: "negative VNI", profile: "external", vni: cutil.GetPtr(-1), wantErr: true},
		{name: "VNI exceeds 24 bits", profile: "external", vni: cutil.GetPtr(maxVpcRoutingVni + 1), wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := (&APIVpcRoutingProfileUpdateRequest{RoutingProfile: tt.profile, Vni: tt.vni}).Validate()
			if tt.wantErr {
				require.Error(t, err)
				assert.IsType(t, validation.Errors{}, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPIVpcInactiveVniReleaseRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		version string
		vni     *int
		wantErr bool
	}{
		{name: "original observation", version: "V12-T1789142400000000", vni: cutil.GetPtr(51000)},
		{name: "maximum counter", version: "V18446744073709551615-T0", vni: cutil.GetPtr(maxVpcRoutingVni)},
		{name: "missing version", vni: cutil.GetPtr(1), wantErr: true},
		{name: "malformed version", version: "V1-T-1", vni: cutil.GetPtr(1), wantErr: true},
		{name: "zero counter", version: "V0-T1", vni: cutil.GetPtr(1), wantErr: true},
		{name: "counter overflow", version: "V18446744073709551616-T1", vni: cutil.GetPtr(1), wantErr: true},
		{name: "timestamp overflow", version: "V1-T18446744073709551616", vni: cutil.GetPtr(1), wantErr: true},
		{name: "missing VNI", version: "V1-T1", wantErr: true},
		{name: "zero VNI", version: "V1-T1", vni: cutil.GetPtr(0), wantErr: true},
		{name: "VNI exceeds 24 bits", version: "V1-T1", vni: cutil.GetPtr(maxVpcRoutingVni + 1), wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := (&APIVpcInactiveVniReleaseRequest{IfVersionMatch: tt.version, ExpectedInactiveVni: tt.vni}).Validate()
			if tt.wantErr {
				require.Error(t, err)
				assert.IsType(t, validation.Errors{}, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAPIVpcRoutingProfileUpdateRequest_ToProto(t *testing.T) {
	tests := []struct {
		name, profile, expectedProfile string
		vni                            *int
	}{
		{name: "automatic custom name", profile: "tenant_external_2", expectedProfile: "tenant_external_2"},
		{name: "exact known alias", profile: "external", expectedProfile: "EXTERNAL", vni: cutil.GetPtr(51000)},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := APIVpcRoutingProfileUpdateRequest{VpcID: routingVpcID, IfVersionMatch: "V12-T34", RoutingProfile: tt.profile, Vni: tt.vni}
			raw := req.ToProto()
			assert.Equal(t, routingVpcID, raw.GetId().GetValue())
			assert.Equal(t, &req.IfVersionMatch, raw.IfVersionMatch)
			assert.Equal(t, tt.expectedProfile, raw.RoutingProfileType)
			assert.Equal(t, cutil.IntPtrToUint32Ptr(tt.vni), raw.Vni)
			encoded, err := json.Marshal(req)
			require.NoError(t, err)
			assert.NotContains(t, string(encoded), "ifVersionMatch")
			assert.NotContains(t, string(encoded), "vpcId")
		})
	}
}

func TestAPIVpcInactiveVniReleaseRequest_ToProto(t *testing.T) {
	req := APIVpcInactiveVniReleaseRequest{VpcID: routingVpcID, IfVersionMatch: "V12-T34", ExpectedInactiveVni: cutil.GetPtr(51000)}
	raw := req.ToProto()
	assert.Equal(t, routingVpcID, raw.GetId().GetValue())
	assert.Equal(t, &req.IfVersionMatch, raw.IfVersionMatch)
	assert.Equal(t, cutil.GetPtr(uint32(51000)), raw.ExpectedInactiveVni)
}

func TestAPIVpcRoutingState_FromProto(t *testing.T) {
	tests := []struct {
		name string
		raw  *corev1.VpcRoutingState
		want string
	}{
		{name: "absent fields stay explicit", raw: &corev1.VpcRoutingState{Id: &corev1.VpcId{Value: routingVpcID}, Version: "V1-T2"},
			want: `{"vpcId":"a8545cb9-97b8-475a-befb-94cab9c96606","version":"V1-T2","routingProfile":null,"activeVni":0,"retainedAllocation":null}`},
		{name: "normalized profile and retained allocation", raw: routingStateProto(),
			want: `{"vpcId":"a8545cb9-97b8-475a-befb-94cab9c96606","version":"V13-T33","routingProfile":"external","activeVni":51000,"retainedAllocation":{"poolName":"vpc-vni","vni":10000}}`},
		{name: "nil resets receiver", want: `{"vpcId":"","version":"","routingProfile":null,"activeVni":0,"retainedAllocation":null}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			state := APIVpcRoutingState{VpcID: "old", RoutingProfile: cutil.GetPtr("old")}
			state.FromProto(tt.raw)
			encoded, err := json.Marshal(state)
			require.NoError(t, err)
			assert.JSONEq(t, tt.want, string(encoded))
		})
	}
}

func TestAPIVpcRoutingState_ValidateResponse(t *testing.T) {
	tests := []struct {
		name    string
		change  func(*APIVpcRoutingState)
		wantErr bool
	}{
		{name: "valid retained allocation", change: func(*APIVpcRoutingState) {}},
		{name: "inspect stored zero and large VNI", change: func(s *APIVpcRoutingState) {
			s.ActiveVni = 0
			s.RetainedAllocation.Vni = maxVpcRoutingVni + 1
		}},
		{name: "missing identity", change: func(s *APIVpcRoutingState) { s.VpcID = "" }, wantErr: true},
		{name: "wrong identity", change: func(s *APIVpcRoutingState) { s.VpcID = "another-vpc" }, wantErr: true},
		{name: "invalid version", change: func(s *APIVpcRoutingState) { s.Version = "V0-T0" }, wantErr: true},
		{name: "unknown pool", change: func(s *APIVpcRoutingState) { s.RetainedAllocation.PoolName = "other" }, wantErr: true},
		{name: "retained equals active", change: func(s *APIVpcRoutingState) { s.RetainedAllocation.Vni = s.ActiveVni }, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			state := &APIVpcRoutingState{}
			state.FromProto(routingStateProto())
			tt.change(state)
			err := state.ValidateResponse(routingVpcID)
			assert.Equal(t, tt.wantErr, err != nil, "%v", err)
		})
	}
}

func TestAPIVpcRoutingProfileUpdateRequest_ValidateResponse(t *testing.T) {
	tests := []struct {
		name    string
		change  func(*APIVpcRoutingProfileUpdateRequest, *corev1.VpcRoutingState, *APIVpcRoutingState)
		wantErr bool
	}{
		{name: "exact VNI with earlier timestamp", change: func(*APIVpcRoutingProfileUpdateRequest, *corev1.VpcRoutingState, *APIVpcRoutingState) {}},
		{name: "custom configured name", change: func(r *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			r.RoutingProfile = "tenant_external_2"
			s.RoutingProfileType = &r.RoutingProfile
		}},
		{name: "counter wraps without zero", change: func(r *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			r.IfVersionMatch = "V18446744073709551615-T34"
			s.Version = "V1-T33"
		}},
		{name: "reuse retained allocation", change: func(r *APIVpcRoutingProfileUpdateRequest, _ *corev1.VpcRoutingState, before *APIVpcRoutingState) {
			r.Vni = nil
			before.RetainedAllocation = &APIVpcRetainedVniAllocation{PoolName: "external-vpc-vni", Vni: 51000}
		}},
		{name: "wrong exact VNI", change: func(r *APIVpcRoutingProfileUpdateRequest, _ *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			r.Vni = cutil.GetPtr(51001)
		}, wantErr: true},
		{name: "raw profile differs despite identical REST alias", change: func(_ *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			s.RoutingProfileType = cutil.GetPtr("external")
		}, wantErr: true},
		{name: "missing profile", change: func(_ *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			s.RoutingProfileType = nil
		}, wantErr: true},
		{name: "unchanged counter", change: func(_ *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			s.Version = "V12-T35"
		}, wantErr: true},
		{name: "missing retained allocation", change: func(_ *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			s.RetainedAllocation = nil
		}, wantErr: true},
		{name: "wrong previous active VNI", change: func(_ *APIVpcRoutingProfileUpdateRequest, _ *corev1.VpcRoutingState, before *APIVpcRoutingState) {
			before.ActiveVni = 10001
		}, wantErr: true},
		{name: "out of range active VNI", change: func(_ *APIVpcRoutingProfileUpdateRequest, s *corev1.VpcRoutingState, _ *APIVpcRoutingState) {
			s.ActiveVni = 0
		}, wantErr: true},
		{name: "retained allocation not reused", change: func(_ *APIVpcRoutingProfileUpdateRequest, _ *corev1.VpcRoutingState, before *APIVpcRoutingState) {
			before.RetainedAllocation = &APIVpcRetainedVniAllocation{PoolName: "external-vpc-vni", Vni: 51001}
		}, wantErr: true},
		{name: "retained pool did not change", change: func(_ *APIVpcRoutingProfileUpdateRequest, _ *corev1.VpcRoutingState, before *APIVpcRoutingState) {
			before.RetainedAllocation = &APIVpcRetainedVniAllocation{PoolName: "vpc-vni", Vni: 51000}
		}, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := &APIVpcRoutingProfileUpdateRequest{VpcID: routingVpcID, IfVersionMatch: "V12-T34", RoutingProfile: "external", Vni: cutil.GetPtr(51000)}
			state := routingStateProto()
			observed := &APIVpcRoutingState{ActiveVni: 10000}
			tt.change(req, state, observed)
			err := req.ValidateResponse(state, observed)
			assert.Equal(t, tt.wantErr, err != nil, "%v", err)
		})
	}
}

func TestAPIVpcInactiveVniReleaseRequest_ValidateResponse(t *testing.T) {
	tests := []struct {
		name    string
		change  func(*corev1.VpcReleaseInactiveVniResult)
		wantErr bool
	}{
		{name: "committed release", change: func(*corev1.VpcReleaseInactiveVniResult) {}},
		{name: "no profile and zero active VNI", change: func(r *corev1.VpcReleaseInactiveVniResult) {
			r.Vpc.Config.RoutingProfileType = nil
			r.Vpc.Status.Vni = cutil.GetPtr(uint32(0))
		}},
		{name: "large stored active VNI", change: func(r *corev1.VpcReleaseInactiveVniResult) {
			r.Vpc.Status.Vni = cutil.GetPtr(uint32(maxVpcRoutingVni + 1))
		}},
		{name: "missing VPC", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc = nil }, wantErr: true},
		{name: "wrong VPC", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc.Id.Value = "another-vpc" }, wantErr: true},
		{name: "missing config", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc.Config = nil }, wantErr: true},
		{name: "missing status", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc.Status = nil }, wantErr: true},
		{name: "omitted active VNI is not zero", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc.Status.Vni = nil }, wantErr: true},
		{name: "wrong version", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc.Version = "V14-T35" }, wantErr: true},
		{name: "wrong released VNI", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.ReleasedInactiveVni = 10001 }, wantErr: true},
		{name: "released active VNI", change: func(r *corev1.VpcReleaseInactiveVniResult) { r.Vpc.Status.Vni = cutil.GetPtr(uint32(10000)) }, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := &APIVpcInactiveVniReleaseRequest{VpcID: routingVpcID, IfVersionMatch: "V12-T34", ExpectedInactiveVni: cutil.GetPtr(10000)}
			raw := routingReleaseProto()
			tt.change(raw)
			err := req.ValidateResponse(raw)
			assert.Equal(t, tt.wantErr, err != nil, "%v", err)
		})
	}
}

func TestAPIVpcInactiveVniReleaseResult_FromProto(t *testing.T) {
	tests := []struct {
		name    string
		profile *string
		want    *string
	}{
		{name: "known alias", profile: cutil.GetPtr("EXTERNAL"), want: cutil.GetPtr("external")},
		{name: "custom name", profile: cutil.GetPtr("tenant_external_2"), want: cutil.GetPtr("tenant_external_2")},
		{name: "absent profile"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			raw := routingReleaseProto()
			raw.Vpc.Config.RoutingProfileType = tt.profile
			result := &APIVpcInactiveVniReleaseResult{}
			result.FromProto(raw)
			assert.Equal(t, APIVpcInactiveVniReleaseResult{VpcID: routingVpcID, Version: "V13-T33", RoutingProfile: tt.want, ActiveVni: 51000, ReleasedInactiveVni: 10000}, *result)
		})
	}
}

func routingStateProto() *corev1.VpcRoutingState {
	return &corev1.VpcRoutingState{
		Id: &corev1.VpcId{Value: routingVpcID}, Version: "V13-T33",
		RoutingProfileType: cutil.GetPtr("EXTERNAL"), ActiveVni: 51000,
		RetainedAllocation: &corev1.VpcRetainedVniAllocation{PoolName: "vpc-vni", Vni: 10000},
	}
}

func routingReleaseProto() *corev1.VpcReleaseInactiveVniResult {
	return &corev1.VpcReleaseInactiveVniResult{
		Vpc: &corev1.Vpc{
			Id: &corev1.VpcId{Value: routingVpcID}, Version: "V13-T33",
			Config: &corev1.VpcConfig{RoutingProfileType: cutil.GetPtr("EXTERNAL")},
			Status: &corev1.VpcStatus{Vni: cutil.GetPtr(uint32(51000))},
		},
		ReleasedInactiveVni: 10000,
	}
}

func TestAPIVpcRoutingProfileDefinition_FromProto(t *testing.T) {
	cases := []struct {
		name   string
		config *corev1.NamedRoutingProfileConfig
		want   string
	}{
		{
			name:   "unset properties remain null",
			config: &corev1.NamedRoutingProfileConfig{},
			want:   `{"routeTargetImports":null,"routeTargetsOnExports":null,"leakDefaultRouteFromUnderlay":null,"leakTenantHostRoutesToUnderlay":null,"tenantLeakCommunitiesAccepted":null,"acceptedLeaksFromUnderlay":null,"allowedAnycastPrefixes":null,"internal":null,"accessTier":null,"tenantPrefixOverlapEligible":false}`,
		},
		{
			name: "full definition preserves explicit values and empty lists",
			config: &corev1.NamedRoutingProfileConfig{
				RouteTargetImports: &corev1.RouteTargets{Values: []*corev1.RouteTarget{
					{
						Asn: 64512,
						Vni: cutil.GetPtr(uint32(0)),
					},
					{
						Asn: 64514,
					},
					{
						Asn:     64513,
						VniSelf: true,
					},
					{
						Vni: cutil.GetPtr(uint32(12345)),
					},
				}},
				RouteTargetsOnExports:          &corev1.RouteTargets{},
				LeakDefaultRouteFromUnderlay:   cutil.GetPtr(false),
				LeakTenantHostRoutesToUnderlay: cutil.GetPtr(true),
				TenantLeakCommunitiesAccepted:  cutil.GetPtr(false),
				AcceptedLeaksFromUnderlay:      &corev1.PrefixFilterPolicyEntries{Values: []*corev1.PrefixFilterPolicyEntry{{Prefix: "192.0.2.0/24"}}},
				AllowedAnycastPrefixes:         &corev1.PrefixFilterPolicyEntries{},
				Internal:                       cutil.GetPtr(false),
				AccessTier:                     cutil.GetPtr(uint32(0)),
				TenantPrefixOverlapEligible:    true,
			},
			want: `{"routeTargetImports":[{"asn":64512,"vni":0,"vniSelf":false},{"asn":64514,"vniSelf":false},{"asn":64513,"vniSelf":true},{"asn":0,"vni":12345,"vniSelf":false}],"routeTargetsOnExports":[],"leakDefaultRouteFromUnderlay":false,"leakTenantHostRoutesToUnderlay":true,"tenantLeakCommunitiesAccepted":false,"acceptedLeaksFromUnderlay":["192.0.2.0/24"],"allowedAnycastPrefixes":[],"internal":false,"accessTier":0,"tenantPrefixOverlapEligible":true}`,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var profile APIVpcRoutingProfileDefinition
			profile.FromProto(&corev1.NamedRoutingProfile{
				Name:   "exact-name",
				Config: tc.config,
			})
			assert.Equal(t, "exact-name", profile.Name)
			data, err := json.Marshal(profile.Config)
			require.NoError(t, err)
			assert.JSONEq(t, tc.want, string(data))

			// Exercise the generated GET decoder with the actual server response.
			encoded, err := json.Marshal(profile)
			require.NoError(t, err)
			const siteID = "61b92164-4314-448c-a6b5-53b036df0739"
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, http.MethodGet, r.Method)
				assert.Equal(t, "/v2/org/test-org/nico/vpc/routing-profile/exact-name", r.URL.Path)
				assert.Equal(t, siteID, r.URL.Query().Get("siteId"))
				w.Header().Set("Content-Type", "application/json")
				_, err := w.Write(encoded)
				assert.NoError(t, err)
			}))
			t.Cleanup(server.Close)
			config := standard.NewConfiguration()
			config.Servers = standard.ServerConfigurations{{URL: server.URL}}
			client := standard.NewAPIClient(config)
			decoded, response, err := client.VPCAPI.GetVpcRoutingProfileDefinition(context.Background(), "test-org", profile.Name).SiteId(siteID).Execute()
			require.NoError(t, err)
			require.NotNil(t, response)
			assert.Equal(t, http.StatusOK, response.StatusCode)
			t.Cleanup(func() { _ = response.Body.Close() })
			require.NotNil(t, decoded)
			assert.Equal(t, profile.Name, decoded.Name)
			assert.True(t, decoded.Config.Internal.IsSet())
			assert.Equal(t, profile.Config.Internal, decoded.Config.Internal.Get())
			assert.True(t, decoded.Config.AccessTier.IsSet())
			if profile.Config.AccessTier == nil {
				assert.Nil(t, decoded.Config.AccessTier.Get())
			} else {
				require.NotNil(t, decoded.Config.AccessTier.Get())
				assert.Equal(t, int64(*profile.Config.AccessTier), *decoded.Config.AccessTier.Get())
			}
			assert.Equal(t, profile.Config.TenantPrefixOverlapEligible, decoded.Config.TenantPrefixOverlapEligible)
			profile.FromProto(nil)
			assert.Empty(t, profile)
		})
	}
}

func TestVpcRoutingProfileDefinitionConfig_UnmarshalJSON(t *testing.T) {
	cases := []struct {
		name    string
		payload string
		field   string
	}{
		{
			name:    "missing internal",
			payload: `{"accessTier":0,"tenantPrefixOverlapEligible":false}`,
			field:   "internal",
		},
		{
			name:    "missing access tier",
			payload: `{"internal":false,"tenantPrefixOverlapEligible":false}`,
			field:   "accessTier",
		},
		{
			name:    "null nonnullable property",
			payload: `{"internal":null,"accessTier":null,"tenantPrefixOverlapEligible":null}`,
			field:   "tenantPrefixOverlapEligible",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var config standard.VpcRoutingProfileDefinitionConfig
			assert.EqualError(t, json.Unmarshal([]byte(tc.payload), &config), "no value given for required property "+tc.field)
		})
	}
}

func TestAPIVpcRoutingProfileGetAllRequest_Validate(t *testing.T) {
	cases := []struct {
		name   string
		siteID string
		valid  bool
	}{
		{
			name: "missing Site",
		},
		{
			name:   "malformed Site",
			siteID: "invalid",
		},
		{
			name:   "valid Site",
			siteID: "61b92164-4314-448c-a6b5-53b036df0739",
			valid:  true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := APIVpcRoutingProfileGetAllRequest{SiteID: tc.siteID}
			assert.Equal(t, tc.valid, req.Validate() == nil)
		})
	}
}

func TestAPIVpcRoutingProfileGetAllRequest_ToProto(t *testing.T) {
	cases := []struct {
		name    string
		request APIVpcRoutingProfileGetAllRequest
		want    *corev1.RoutingProfileSearchFilter
	}{
		{
			name: "omitted",
			want: &corev1.RoutingProfileSearchFilter{},
		},
		{
			name: "false and zero",
			request: APIVpcRoutingProfileGetAllRequest{
				Internal:          cutil.GetPtr(false),
				MinimumAccessTier: cutil.GetPtr(uint32(0)),
			},
			want: &corev1.RoutingProfileSearchFilter{
				Internal:          cutil.GetPtr(false),
				MinimumAccessTier: cutil.GetPtr(uint32(0)),
			},
		},
		{
			name: "true and maximum tier",
			request: APIVpcRoutingProfileGetAllRequest{
				Internal:          cutil.GetPtr(true),
				MinimumAccessTier: cutil.GetPtr(uint32(4294967295)),
			},
			want: &corev1.RoutingProfileSearchFilter{
				Internal:          cutil.GetPtr(true),
				MinimumAccessTier: cutil.GetPtr(uint32(4294967295)),
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.True(t, proto.Equal(tc.want, tc.request.ToProto()))
		})
	}
}
