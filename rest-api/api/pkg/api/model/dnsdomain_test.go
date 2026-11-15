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

const (
	testDomainSiteID = "00000000-0000-0000-0000-000000000001"
	testDomainID     = "00000000-0000-0000-0000-000000000002"
)

func TestAPIDNSDomainCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIDNSDomainCreateRequest
		wantErr string
	}{
		{name: "name only", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com"}},
		{name: "lowest TTL", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com", DefaultTTL: cutil.GetPtr(DNSDomainDefaultTTLMin)}},
		{name: "highest TTL", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com", DefaultTTL: cutil.GetPtr(DNSDomainDefaultTTLMax)}},
		{name: "missing Site", req: APIDNSDomainCreateRequest{Name: "tenant-a.example.com"}, wantErr: "siteId"},
		{name: "invalid Site", req: APIDNSDomainCreateRequest{SiteID: "invalid", Name: "tenant-a.example.com"}, wantErr: "siteId"},
		{name: "missing name", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID}, wantErr: "name"},
		{name: "zero TTL", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com", DefaultTTL: cutil.GetPtr(0)}, wantErr: "defaultTtl: must be no less than 30"},
		{name: "TTL below range", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com", DefaultTTL: cutil.GetPtr(DNSDomainDefaultTTLMin - 1)}, wantErr: "defaultTtl"},
		{name: "TTL above range", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com", DefaultTTL: cutil.GetPtr(DNSDomainDefaultTTLMax + 1)}, wantErr: "defaultTtl"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestAPIDNSDomainCreateRequest_ToProto(t *testing.T) {
	tests := []struct {
		name    string
		req     APIDNSDomainCreateRequest
		wantTTL *uint32
	}{
		{name: "TTL set", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com", DefaultTTL: cutil.GetPtr(600)}, wantTTL: cutil.GetPtr(uint32(600))},
		{name: "TTL omitted stays absent", req: APIDNSDomainCreateRequest{SiteID: testDomainSiteID, Name: "tenant-a.example.com"}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			protoReq := tt.req.ToProto()
			assert.Equal(t, tt.req.Name, protoReq.GetName())
			assert.Equal(t, tt.wantTTL, protoReq.DefaultTtl)
		})
	}
}

func TestAPIDNSDomainGetAllRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIDNSDomainGetAllRequest
		wantErr string
	}{
		{name: "valid", req: APIDNSDomainGetAllRequest{SiteID: testDomainSiteID}},
		{name: "missing Site", req: APIDNSDomainGetAllRequest{}, wantErr: "siteId: "},
		{name: "invalid Site", req: APIDNSDomainGetAllRequest{SiteID: "invalid"}, wantErr: "siteId: "},
		{name: "valid name filter", req: APIDNSDomainGetAllRequest{SiteID: testDomainSiteID, Name: cutil.GetPtr("tenant-a.example.com")}},
		{name: "empty name filter", req: APIDNSDomainGetAllRequest{SiteID: testDomainSiteID, Name: cutil.GetPtr("")}, wantErr: "name: "},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestAPIDNSDomainGetAllRequest_ToProto(t *testing.T) {
	all := APIDNSDomainGetAllRequest{SiteID: testDomainSiteID}
	assert.Nil(t, all.ToProto().Name, "no name filter lists every Domain")
	assert.Nil(t, all.ToProto().Id)

	named := APIDNSDomainGetAllRequest{SiteID: testDomainSiteID, Name: cutil.GetPtr("tenant-a.example.com")}
	assert.Equal(t, "tenant-a.example.com", named.ToProto().GetName())
}

func TestAPIDNSDomainUpdateRequest_Validate(t *testing.T) {
	ttl := func(v int) *int { return &v }
	tests := []struct {
		name    string
		req     APIDNSDomainUpdateRequest
		wantErr string
	}{
		{name: "valid", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: ttl(600)}},
		{name: "lowest TTL", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: ttl(DNSDomainDefaultTTLMin)}},
		{name: "highest TTL", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: ttl(DNSDomainDefaultTTLMax)}},
		{name: "missing TTL", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID}, wantErr: "defaultTtl: "},
		{name: "zero TTL", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: ttl(0)}, wantErr: "defaultTtl: "},
		{name: "TTL below the minimum", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: ttl(DNSDomainDefaultTTLMin - 1)}, wantErr: "defaultTtl: "},
		{name: "TTL above the maximum", req: APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: ttl(DNSDomainDefaultTTLMax + 1)}, wantErr: "defaultTtl: "},
		{name: "missing Site", req: APIDNSDomainUpdateRequest{ID: testDomainID, DefaultTTL: ttl(600)}, wantErr: "siteId: "},
		{name: "invalid ID", req: APIDNSDomainUpdateRequest{ID: "invalid", SiteID: testDomainSiteID, DefaultTTL: ttl(600)}, wantErr: "id: "},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestAPIDNSDomainUpdateRequest_ToProto(t *testing.T) {
	ttl := 600
	req := APIDNSDomainUpdateRequest{ID: testDomainID, SiteID: testDomainSiteID, DefaultTTL: &ttl}
	domain := req.ToProto().GetDomain()
	assert.Equal(t, testDomainID, domain.GetId().GetValue())
	assert.Equal(t, uint32(600), domain.GetDefaultTtl())
	assert.Empty(t, domain.GetName(), "an empty name tells Core to keep the stored name")
}

func TestIsReverseDNSZoneName(t *testing.T) {
	tests := []struct {
		name string
		want bool
	}{
		{name: "254.254.254.169.in-addr.arpa", want: true},
		{name: "8.b.d.0.1.0.0.2.ip6.arpa", want: true},
		{name: "in-addr.arpa", want: true},
		{name: " 2.0.192.IN-ADDR.ARPA. ", want: true},
		{name: "tenant-a.example.com", want: false},
		{name: "notin-addr.arpa", want: false},
		{name: "in-addr.arpa.example.com", want: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, IsReverseDNSZoneName(tt.name))
		})
	}
}

func TestAPIDNSDomainGetRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIDNSDomainGetRequest
		wantErr string
	}{
		{name: "valid", req: APIDNSDomainGetRequest{SiteID: testDomainSiteID, ID: testDomainID}},
		{name: "missing Site", req: APIDNSDomainGetRequest{ID: testDomainID}, wantErr: "siteId: "},
		{name: "invalid Site", req: APIDNSDomainGetRequest{SiteID: "invalid", ID: testDomainID}, wantErr: "siteId: "},
		{name: "invalid ID", req: APIDNSDomainGetRequest{SiteID: testDomainSiteID, ID: "invalid"}, wantErr: "id: "},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestAPIDNSDomainGetRequest_ToProto(t *testing.T) {
	req := APIDNSDomainGetRequest{SiteID: testDomainSiteID, ID: testDomainID}
	protoReq := req.ToProto()
	assert.Equal(t, testDomainID, protoReq.GetId().GetValue())
	assert.Nil(t, protoReq.Name, "a search by ID must not also filter by name")
}

func TestAPIDNSDomainDeleteRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIDNSDomainDeleteRequest
		wantErr string
	}{
		{name: "valid", req: APIDNSDomainDeleteRequest{SiteID: testDomainSiteID, ID: testDomainID}},
		{name: "missing Site", req: APIDNSDomainDeleteRequest{ID: testDomainID}, wantErr: "siteId: "},
		{name: "invalid Site", req: APIDNSDomainDeleteRequest{SiteID: "invalid", ID: testDomainID}, wantErr: "siteId: "},
		{name: "invalid ID", req: APIDNSDomainDeleteRequest{SiteID: testDomainSiteID, ID: "invalid"}, wantErr: "id: "},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestAPIDNSDomainDeleteRequest_ToProto(t *testing.T) {
	req := APIDNSDomainDeleteRequest{SiteID: testDomainSiteID, ID: testDomainID}
	assert.Equal(t, testDomainID, req.ToProto().GetId().GetValue())
}

func TestNewAPIDNSDomain(t *testing.T) {
	created := time.Date(2026, 11, 13, 10, 0, 0, 0, time.UTC)
	updated := created.Add(time.Hour)

	tests := []struct {
		name     string
		domain   *corev1.Domain
		want     *APIDNSDomain
		wantJSON string
	}{
		{name: "nil Domain", domain: nil, want: nil, wantJSON: "null"},
		{
			name: "all fields",
			domain: &corev1.Domain{
				Id:         &corev1.DomainId{Value: testDomainID},
				Name:       "tenant-a.example.com",
				Created:    timestamppb.New(created),
				Updated:    timestamppb.New(updated),
				DefaultTtl: cutil.GetPtr(uint32(600)),
			},
			want: &APIDNSDomain{
				ID:         testDomainID,
				SiteID:     testDomainSiteID,
				Name:       "tenant-a.example.com",
				DefaultTTL: cutil.GetPtr(600),
				Created:    &created,
				Updated:    &updated,
			},
			wantJSON: `{"id":"` + testDomainID + `","siteId":"` + testDomainSiteID + `","name":"tenant-a.example.com","defaultTtl":600,"created":"2026-11-13T10:00:00Z","updated":"2026-11-13T11:00:00Z"}`,
		},
		{
			name:     "unset fields stay in the response as null",
			domain:   &corev1.Domain{Id: &corev1.DomainId{Value: testDomainID}, Name: "tenant-a.example.com"},
			want:     &APIDNSDomain{ID: testDomainID, SiteID: testDomainSiteID, Name: "tenant-a.example.com"},
			wantJSON: `{"id":"` + testDomainID + `","siteId":"` + testDomainSiteID + `","name":"tenant-a.example.com","defaultTtl":null,"created":null,"updated":null}`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIDNSDomain(tt.domain, testDomainSiteID)
			assert.Equal(t, tt.want, got)

			data, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, tt.wantJSON, string(data))
		})
	}
}
