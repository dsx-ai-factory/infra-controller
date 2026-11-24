// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIDhcpOptionSetCreateRequest_Validate(t *testing.T) {
	valid := func() APIDhcpOptionSetCreateRequest {
		return APIDhcpOptionSetCreateRequest{
			Name:   "pxe-boot",
			SiteID: uuid.NewString(),
			IPv4:   &APIDhcpOptions{BootURL: cutil.GetPtr("http://10.0.0.5/boot.ipxe"), DNSServers: []string{"10.0.0.53"}},
			IPv6:   &APIDhcpOptions{DNSServers: []string{"fd00::53"}},
		}
	}

	tests := []struct {
		name    string
		modify  func(r *APIDhcpOptionSetCreateRequest)
		wantErr bool
	}{
		{name: "test validation success", modify: func(r *APIDhcpOptionSetCreateRequest) {}},
		{name: "test validation success, only an IPv6 DNS server", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv4 = nil }},
		{name: "test validation failure, missing name", modify: func(r *APIDhcpOptionSetCreateRequest) { r.Name = "" }, wantErr: true},
		{name: "test validation failure, invalid siteId", modify: func(r *APIDhcpOptionSetCreateRequest) { r.SiteID = "bad" }, wantErr: true},
		{name: "test validation failure, non-http boot URL", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv4.BootURL = cutil.GetPtr("tftp://10.0.0.5/boot") }, wantErr: true},
		{name: "test validation failure, boot URL with credentials", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv4.BootURL = cutil.GetPtr("http://u:p@10.0.0.5/boot.ipxe") }, wantErr: true},
		{name: "test validation failure, boot URL with empty hostname", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv4.BootURL = cutil.GetPtr("http://:80/boot.ipxe") }, wantErr: true},
		{name: "test validation failure, IPv6 address in IPv4 DNS servers", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv4.DNSServers = []string{"fd00::53"} }, wantErr: true},
		{name: "test validation failure, IPv4 address in IPv6 DNS servers", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv6.DNSServers = []string{"10.0.0.53"} }, wantErr: true},
		{name: "test validation failure, no value set", modify: func(r *APIDhcpOptionSetCreateRequest) { r.IPv4, r.IPv6 = nil, &APIDhcpOptions{} }, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := valid()
			tt.modify(&r)
			err := r.Validate()
			assert.Equal(t, tt.wantErr, err != nil, "err: %v", err)
		})
	}
}

func TestAPIDhcpOptionSetUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIDhcpOptionSetUpdateRequest
		wantErr bool
	}{
		{name: "test validation success, empty update", request: APIDhcpOptionSetUpdateRequest{}},
		{name: "test validation success, clearing values", request: APIDhcpOptionSetUpdateRequest{IPv4: &APIDhcpOptions{BootURL: cutil.GetPtr(""), DNSServers: []string{}}}},
		{name: "test validation failure, name too short", request: APIDhcpOptionSetUpdateRequest{Name: cutil.GetPtr("a")}, wantErr: true},
		{name: "test validation failure, non-http boot URL", request: APIDhcpOptionSetUpdateRequest{IPv6: &APIDhcpOptions{BootURL: cutil.GetPtr("not a url")}}, wantErr: true},
		{name: "test validation failure, boot URL with credentials", request: APIDhcpOptionSetUpdateRequest{IPv6: &APIDhcpOptions{BootURL: cutil.GetPtr("http://user@[fd00::5]/boot.ipxe")}}, wantErr: true},
		{name: "test validation failure, invalid DNS server", request: APIDhcpOptionSetUpdateRequest{IPv4: &APIDhcpOptions{DNSServers: []string{"10.0.0.300"}}}, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil, "err: %v", err)
		})
	}
}

func TestAPIDhcpOptionSetUpdateRequest_mergeOptions(t *testing.T) {
	stored := &cdbm.DhcpOptions{DhcpOptions: &corev1.DhcpOptions{
		V4: &corev1.DhcpOptionsV4{BootUrl: cutil.GetPtr("http://10.0.0.5/boot.ipxe"), DnsServers: []string{"10.0.0.53"}},
		V6: &corev1.DhcpOptionsV6{DnsServers: []string{"fd00::53"}},
	}}

	tests := []struct {
		name    string
		current *cdbm.DhcpOptions
		request APIDhcpOptionSetUpdateRequest
		want    *corev1.DhcpOptions
	}{
		{
			name:    "omitted values keep the stored ones",
			current: stored,
			want:    stored.DhcpOptions,
		},
		{
			name:    "sent values replace the stored ones and empty values clear them",
			current: stored,
			request: APIDhcpOptionSetUpdateRequest{
				IPv4: &APIDhcpOptions{BootURL: cutil.GetPtr("")},
				IPv6: &APIDhcpOptions{BootURL: cutil.GetPtr("https://boot.example.com/v6"), DNSServers: []string{}},
			},
			want: &corev1.DhcpOptions{
				V4: &corev1.DhcpOptionsV4{DnsServers: []string{"10.0.0.53"}},
				V6: &corev1.DhcpOptionsV6{BootUrl: cutil.GetPtr("https://boot.example.com/v6"), DnsServers: []string{}},
			},
		},
		{
			name:    "no stored options",
			request: APIDhcpOptionSetUpdateRequest{IPv4: &APIDhcpOptions{DNSServers: []string{"10.0.0.54"}}},
			want:    &corev1.DhcpOptions{V4: &corev1.DhcpOptionsV4{DnsServers: []string{"10.0.0.54"}}},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.request.mergeOptions(tt.current)
			assert.True(t, proto.Equal(tt.want, got.DhcpOptions), "got: %v", got.DhcpOptions)
		})
	}
	// The stored options are not modified
	assert.Equal(t, "http://10.0.0.5/boot.ipxe", stored.GetV4().GetBootUrl())
}

func TestAPIDhcpOptionSetUpdateRequest_ToProto(t *testing.T) {
	dos := &cdbm.DhcpOptionSet{
		ID:        uuid.New(),
		Name:      "pxe-boot",
		TenantOrg: "test-tenant-org",
		Options:   &cdbm.DhcpOptions{DhcpOptions: &corev1.DhcpOptions{V4: &corev1.DhcpOptionsV4{DnsServers: []string{"10.0.0.53"}}}},
	}

	tests := []struct {
		name        string
		version     string
		wantVersion *string
	}{
		{name: "stored version guards the update", version: "V1-T1", wantVersion: cutil.GetPtr("V1-T1")},
		{name: "no stored version sends no guard", version: ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dos.Version = tt.version
			got := (&APIDhcpOptionSetUpdateRequest{}).ToProto(dos)
			assert.True(t, proto.Equal(&corev1.UpdateDhcpOptionSetRequest{
				Id:                   &corev1.DhcpOptionSetId{Value: dos.ID.String()},
				TenantOrganizationId: "test-tenant-org",
				Metadata:             &corev1.Metadata{Name: "pxe-boot"},
				Options:              dos.Options.DhcpOptions,
				IfVersionMatch:       tt.wantVersion,
			}, got), "got: %v", got)
		})
	}
}

func TestAPIDhcpOptionSet_FromDBModel(t *testing.T) {
	id := uuid.New()

	tests := []struct {
		name     string
		dos      *cdbm.DhcpOptionSet
		wantIPv4 APIDhcpOptions
		wantIPv6 APIDhcpOptions
	}{
		{
			name: "options are mapped per family",
			dos: &cdbm.DhcpOptionSet{
				ID: id, Name: "pxe-boot", Status: cdbm.DhcpOptionSetStatusReady,
				Options: &cdbm.DhcpOptions{DhcpOptions: &corev1.DhcpOptions{
					V4: &corev1.DhcpOptionsV4{BootUrl: cutil.GetPtr("http://10.0.0.5/boot.ipxe"), DnsServers: []string{"10.0.0.53"}},
				}},
			},
			wantIPv4: APIDhcpOptions{BootURL: cutil.GetPtr("http://10.0.0.5/boot.ipxe"), DNSServers: []string{"10.0.0.53"}},
			wantIPv6: APIDhcpOptions{DNSServers: []string{}},
		},
		{
			name:     "no options yields empty values rather than omitted ones",
			dos:      &cdbm.DhcpOptionSet{ID: id, Name: "pxe-boot", Status: cdbm.DhcpOptionSetStatusPending},
			wantIPv4: APIDhcpOptions{DNSServers: []string{}},
			wantIPv6: APIDhcpOptions{DNSServers: []string{}},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := &APIDhcpOptionSet{}
			got.FromDBModel(tt.dos, []cdbm.StatusDetail{{EntityID: id.String(), Status: string(tt.dos.Status)}})
			assert.Equal(t, id.String(), got.ID)
			assert.Equal(t, "pxe-boot", got.Name)
			assert.Equal(t, tt.dos.Status, got.Status)
			assert.Equal(t, tt.wantIPv4, got.IPv4)
			assert.Equal(t, tt.wantIPv6, got.IPv6)
			assert.Len(t, got.StatusHistory, 1)
			assert.Nil(t, got.Site)
			assert.Nil(t, got.Tenant)
		})
	}
}
