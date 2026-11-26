// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const testAttestationSiteID = "00000000-0000-0000-0000-000000000001"

func allowlist(matchers ...APIComponentIDMatch) *APIAttesterSelection {
	return &APIAttesterSelection{Mode: AttesterSelectionModeAllowlist, ComponentIDs: matchers}
}

func exact(s string) APIComponentIDMatch  { return APIComponentIDMatch{Exact: &s} }
func prefix(s string) APIComponentIDMatch { return APIComponentIDMatch{Prefix: &s} }

func TestAPIAttestationProfileCreateRequest_Validate(t *testing.T) {
	valid := APIAttestationProfileCreateRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", Selection: allowlist(exact("VERA_CPU_0"))}
	cases := []struct {
		name    string
		mutate  func(*APIAttestationProfileCreateRequest)
		wantErr string
	}{
		{name: "valid class", mutate: func(*APIAttestationProfileCreateRequest) {}},
		{name: "any class", mutate: func(r *APIAttestationProfileCreateRequest) { r.HardwareClass = AttestationAnyHardwareClass }},
		{name: "any set to None needs no acknowledgement", mutate: func(r *APIAttestationProfileCreateRequest) {
			r.HardwareClass = AttestationAnyHardwareClass
			r.Selection = &APIAttesterSelection{Mode: AttesterSelectionModeNone}
		}},
		{name: "literal backslash u0000 is allowed", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection = allowlist(exact(`VERA\u0000`)) }},
		{name: "missing site", mutate: func(r *APIAttestationProfileCreateRequest) { r.SiteID = "" }, wantErr: "siteId"},
		{name: "invalid class", mutate: func(r *APIAttestationProfileCreateRequest) { r.HardwareClass = "NVIDIA_gb200" }, wantErr: "hardwareClass"},
		{name: "class with three fields", mutate: func(r *APIAttestationProfileCreateRequest) { r.HardwareClass = "a_b_c" }, wantErr: "hardwareClass"},
		{name: "missing selection", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection = nil }, wantErr: "selection"},
		{name: "unknown mode", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection.Mode = "Some" }, wantErr: "mode"},
		{name: "allowlist without matchers", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection = allowlist() }, wantErr: "componentIds"},
		{name: "All with matchers", mutate: func(r *APIAttestationProfileCreateRequest) {
			r.Selection = &APIAttesterSelection{Mode: AttesterSelectionModeAll, ComponentIDs: []APIComponentIDMatch{exact("x")}}
		}, wantErr: "componentIds"},
		{name: "matcher with both arms", mutate: func(r *APIAttestationProfileCreateRequest) {
			a, b := "a", "b"
			r.Selection = allowlist(APIComponentIDMatch{Exact: &a, Prefix: &b})
		}, wantErr: "exactly one"},
		{name: "matcher with no arm", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection = allowlist(APIComponentIDMatch{}) }, wantErr: "exactly one"},
		{name: "empty prefix", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection = allowlist(prefix("")) }, wantErr: "prefix"},
		{name: "decoded NUL in prefix", mutate: func(r *APIAttestationProfileCreateRequest) { r.Selection = allowlist(prefix("VERA\x00")) }, wantErr: "U+0000"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := valid
			sel := *valid.Selection
			req.Selection = &sel
			tc.mutate(&req)
			err := req.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tc.wantErr)
		})
	}
}

func TestAPIAttestationProfileCreateRequest_ToProto(t *testing.T) {
	req := APIAttestationProfileCreateRequest{
		SiteID:        testAttestationSiteID,
		HardwareClass: "nvidia_gb200",
		Selection:     &APIAttesterSelection{Mode: AttesterSelectionModeDenylist, ComponentIDs: []APIComponentIDMatch{exact("A"), prefix("B")}},
	}
	p := req.ToProto()
	assert.Equal(t, "nvidia_gb200", p.GetHardwareClass())
	assert.Equal(t, corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_DENYLIST, p.GetSelection().GetMode())
	require.Len(t, p.GetSelection().GetComponentIds(), 2)
	assert.Equal(t, "A", p.GetSelection().GetComponentIds()[0].GetExact())
	assert.Equal(t, "B", p.GetSelection().GetComponentIds()[1].GetPrefix())
}

func TestAPIAttestationProfileUpdateRequest_Validate(t *testing.T) {
	cases := []struct {
		name    string
		req     APIAttestationProfileUpdateRequest
		wantErr string
	}{
		{name: "valid", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", Selection: allowlist(exact("A")), IfVersionMatch: cutil.GetPtr("V1-T1")}},
		{name: "missing version", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", Selection: allowlist(exact("A"))}, wantErr: "ifVersionMatch"},
		{name: "empty version", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", Selection: allowlist(exact("A")), IfVersionMatch: cutil.GetPtr("")}, wantErr: "ifVersionMatch"},
		{name: "decoded NUL in version", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", Selection: allowlist(exact("A")), IfVersionMatch: cutil.GetPtr("V1\x00")}, wantErr: "U+0000"},
		{name: "any to None without acknowledgement", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: AttestationAnyHardwareClass, Selection: &APIAttesterSelection{Mode: AttesterSelectionModeNone}, IfVersionMatch: cutil.GetPtr("V1")}, wantErr: "acknowledgeFallbackImpact"},
		{name: "any to None acknowledged", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: AttestationAnyHardwareClass, Selection: &APIAttesterSelection{Mode: AttesterSelectionModeNone}, IfVersionMatch: cutil.GetPtr("V1"), AcknowledgeFallbackImpact: true}},
		{name: "class to None needs no acknowledgement", req: APIAttestationProfileUpdateRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", Selection: &APIAttesterSelection{Mode: AttesterSelectionModeNone}, IfVersionMatch: cutil.GetPtr("V1")}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				assert.Equal(t, *tc.req.IfVersionMatch, tc.req.ToProto().GetIfVersionMatch())
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tc.wantErr)
		})
	}
}

func TestAPIAttestationProfileDeleteRequest_Validate(t *testing.T) {
	cases := []struct {
		name    string
		req     APIAttestationProfileDeleteRequest
		wantErr string
	}{
		{name: "valid", req: APIAttestationProfileDeleteRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", IfVersionMatch: cutil.GetPtr("V1")}},
		{name: "missing version", req: APIAttestationProfileDeleteRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200"}, wantErr: "ifVersionMatch"},
		{name: "decoded NUL in version", req: APIAttestationProfileDeleteRequest{SiteID: testAttestationSiteID, HardwareClass: "nvidia_gb200", IfVersionMatch: cutil.GetPtr("\x00")}, wantErr: "U+0000"},
		{name: "any without acknowledgement", req: APIAttestationProfileDeleteRequest{SiteID: testAttestationSiteID, HardwareClass: AttestationAnyHardwareClass, IfVersionMatch: cutil.GetPtr("V1")}, wantErr: "acknowledgeFallbackImpact"},
		{name: "any acknowledged", req: APIAttestationProfileDeleteRequest{SiteID: testAttestationSiteID, HardwareClass: AttestationAnyHardwareClass, IfVersionMatch: cutil.GetPtr("V1"), AcknowledgeFallbackImpact: true}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tc.wantErr)
		})
	}
}

func TestAPIAttestationProfile_FromProto(t *testing.T) {
	updated := time.Date(2026, 11, 20, 10, 0, 0, 0, time.UTC)
	valid := func() *corev1.AttestationProfile {
		return &corev1.AttestationProfile{
			HardwareClass: "nvidia_gb200",
			Version:       "V2-T1",
			UpdatedAt:     timestamppb.New(updated),
			UpdatedBy:     "elektra-site-agent",
			Selection: &corev1.AttesterSelection{
				Mode:         corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALLOWLIST,
				ComponentIds: []*corev1.ComponentIdMatch{{Pattern: &corev1.ComponentIdMatch_Exact{Exact: "VERA_CPU_0"}}},
			},
		}
	}
	cases := []struct {
		name    string
		mutate  func(*corev1.AttestationProfile)
		wantErr string
	}{
		{name: "valid", mutate: func(*corev1.AttestationProfile) {}},
		{name: "empty reply", mutate: func(p *corev1.AttestationProfile) { *p = corev1.AttestationProfile{} }, wantErr: "hardware class"},
		{name: "other class", mutate: func(p *corev1.AttestationProfile) { p.HardwareClass = "nvidia_gb300" }, wantErr: "not \"nvidia_gb200\""},
		{name: "no version", mutate: func(p *corev1.AttestationProfile) { p.Version = "" }, wantErr: "version"},
		{name: "no update time", mutate: func(p *corev1.AttestationProfile) { p.UpdatedAt = nil }, wantErr: "update time"},
		{name: "unspecified mode", mutate: func(p *corev1.AttestationProfile) { p.Selection.Mode = 0 }, wantErr: "mode"},
		{name: "unknown mode", mutate: func(p *corev1.AttestationProfile) { p.Selection.Mode = 99 }, wantErr: "mode"},
		{name: "matcher without pattern", mutate: func(p *corev1.AttestationProfile) {
			p.Selection.ComponentIds = []*corev1.ComponentIdMatch{{}}
		}, wantErr: "no pattern"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := valid()
			tc.mutate(p)
			var got APIAttestationProfile
			err := got.FromProtoForClass("nvidia_gb200", p)
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				assert.Equal(t, APIAttestationProfile{}, got)
				return
			}
			require.NoError(t, err)
			body, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, `{
				"hardwareClass":"nvidia_gb200","version":"V2-T1","updatedAt":"2026-11-20T10:00:00Z","updatedBy":"elektra-site-agent",
				"selection":{"mode":"Allowlist","componentIds":[{"exact":"VERA_CPU_0","prefix":null}]}
			}`, string(body))
		})
	}
}

func TestAPIAttestationCoverage_FromProto(t *testing.T) {
	none := corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_NONE
	all := corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALL
	cases := []struct {
		name     string
		reply    *corev1.GetAttestationCoverageResponse
		wantJSON string
		wantErr  string
	}{
		{
			name:     "empty report",
			reply:    &corev1.GetAttestationCoverageResponse{},
			wantJSON: `{"entries":[],"anyProfileMode":null}`,
		},
		{
			name: "keeps zero-count and unclassified entries in order",
			reply: &corev1.GetAttestationCoverageResponse{
				AnyProfileMode: &none,
				Entries: []*corev1.AttestationCoverageEntry{
					{HardwareClass: "nvidia_gb200", ExploredEndpoints: 0, Coverage: corev1.AttestationCoverage_ATTESTATION_COVERAGE_OWN_PROFILE, Mode: &all,
						AttesterSets: []*corev1.AttesterSet{{Digest: "abc", ReportingEndpoints: 0, Attesters: 0}}},
					{HardwareClass: "", ExploredEndpoints: 3, Coverage: corev1.AttestationCoverage_ATTESTATION_COVERAGE_CLASS_NOT_RECORDED},
				},
			},
			wantJSON: `{"anyProfileMode":"None","entries":[
				{"hardwareClass":"nvidia_gb200","exploredEndpoints":0,"coverage":"OwnProfile","mode":"All","attesterSets":[{"digest":"abc","reportingEndpoints":0,"attesters":0}]},
				{"hardwareClass":"","exploredEndpoints":3,"coverage":"ClassNotRecorded","mode":null,"attesterSets":[]}
			]}`,
		},
		{
			name:    "unspecified coverage",
			reply:   &corev1.GetAttestationCoverageResponse{Entries: []*corev1.AttestationCoverageEntry{{HardwareClass: "a_b"}}},
			wantErr: "coverage",
		},
		{
			name:    "unknown fallback mode",
			reply:   &corev1.GetAttestationCoverageResponse{AnyProfileMode: (*corev1.AttesterSelectionMode)(cutil.GetPtr(int32(9)))},
			wantErr: "mode",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIAttestationCoverage
			err := got.FromProto(tc.reply)
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				return
			}
			require.NoError(t, err)
			body, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, tc.wantJSON, string(body))
		})
	}
}
