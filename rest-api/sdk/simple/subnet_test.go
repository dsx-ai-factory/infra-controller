// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestToStandardSubnetCreateRequest(t *testing.T) {
	domainID := "domain-1"
	tests := []struct {
		name               string
		subdomainID        *string
		wantSubdomainID    string
		wantHasSubdomainID bool
	}{
		{name: "unset", subdomainID: nil, wantHasSubdomainID: false},
		{name: "populated", subdomainID: &domainID, wantSubdomainID: domainID, wantHasSubdomainID: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			apiRequest := toStandardSubnetCreateRequest(SubnetCreateRequest{
				Name:         "tenant-net",
				VpcID:        "vpc-1",
				SubdomainID:  test.subdomainID,
				IPv4BlockID:  "block-1",
				PrefixLength: 24,
			})

			assert.Equal(t, test.wantHasSubdomainID, apiRequest.HasSubdomainId())
			assert.Equal(t, test.wantSubdomainID, apiRequest.GetSubdomainId())
			body, err := apiRequest.ToMap()
			require.NoError(t, err)
			if test.wantHasSubdomainID {
				gotSubdomainID, ok := body["subdomainId"].(*string)
				require.True(t, ok)
				require.NotNil(t, gotSubdomainID)
				assert.Equal(t, domainID, *gotSubdomainID)
			} else {
				assert.NotContains(t, body, "subdomainId")
			}
		})
	}
}

func TestToStandardSubnetAttachVpcRequest(t *testing.T) {
	for _, allowReplace := range []bool{false, true} {
		t.Run(map[bool]string{false: "replacement rejected", true: "replacement allowed"}[allowReplace], func(t *testing.T) {
			apiRequest := toStandardSubnetAttachVpcRequest(SubnetAttachVpcRequest{
				VpcID:        "vpc-2",
				AllowReplace: allowReplace,
			})
			assert.Equal(t, "vpc-2", apiRequest.GetVpcId())
			assert.Equal(t, allowReplace, apiRequest.GetAllowReplace())
			body, err := apiRequest.ToMap()
			require.NoError(t, err)
			gotAllowReplace, ok := body["allowReplace"].(*bool)
			require.True(t, ok)
			require.NotNil(t, gotAllowReplace)
			assert.Equal(t, allowReplace, *gotAllowReplace)
		})
	}
}

func TestSubnetManagerCreateAndAttachVpc(t *testing.T) {
	requests := make([]string, 0, 2)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		requests = append(requests, r.Method+" "+r.URL.Path)
		var body map[string]any
		require.NoError(t, json.NewDecoder(r.Body).Decode(&body))
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/v2/org/test-org/nico/subnet":
			assert.Equal(t, "tenant-net", body["name"])
			assert.Equal(t, "vpc-1", body["vpcId"])
			assert.Equal(t, "domain-1", body["subdomainId"])
			assert.Equal(t, "block-1", body["ipv4BlockId"])
			assert.Equal(t, float64(24), body["prefixLength"])
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{"id":"subnet-1","name":"tenant-net","description":null,"siteId":"site-1","vpcId":"vpc-1","subdomainId":"domain-1","controllerNetworkSegmentId":"controller-subnet-1","ipv4Prefix":"10.0.0.0","ipv4BlockId":"block-1","ipv4Gateway":"10.0.0.1","ipv6Prefix":null,"ipv6BlockId":null,"ipv6Gateway":null,"prefixLength":24,"routingType":"Public","status":"Ready","statusHistory":[],"created":"2026-09-02T00:00:00Z","updated":"2026-09-02T00:00:00Z"}`)
		case r.Method == http.MethodPost && r.URL.Path == "/v2/org/test-org/nico/subnet/subnet-1/attach-vpc":
			assert.Equal(t, "vpc-2", body["vpcId"])
			assert.Equal(t, true, body["allowReplace"])
			_, _ = io.WriteString(w, `{"id":"subnet-1","name":"tenant-net","description":null,"siteId":"site-1","vpcId":"vpc-2","subdomainId":"domain-1","controllerNetworkSegmentId":"controller-subnet-1","ipv4Prefix":"10.0.0.0","ipv4BlockId":"block-1","ipv4Gateway":"10.0.0.1","ipv6Prefix":null,"ipv6BlockId":null,"ipv6Gateway":null,"prefixLength":24,"routingType":"Public","status":"Ready","statusHistory":[],"created":"2026-09-02T00:00:00Z","updated":"2026-09-02T00:00:00Z"}`)
		default:
			t.Errorf("unexpected request: %s %s", r.Method, r.URL.Path)
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := newSimpleTestClient(server.URL)
	manager := NewSubnetManager(client)
	domainID := "domain-1"
	created, apiErr := manager.Create(context.Background(), SubnetCreateRequest{
		Name:         "tenant-net",
		VpcID:        "vpc-1",
		SubdomainID:  &domainID,
		IPv4BlockID:  "block-1",
		PrefixLength: 24,
	})
	require.Nil(t, apiErr)
	require.NotNil(t, created)
	assert.Equal(t, "subnet-1", created.GetId())

	attached, apiErr := manager.AttachVpc(context.Background(), "subnet-1", SubnetAttachVpcRequest{
		VpcID:        "vpc-2",
		AllowReplace: true,
	})
	require.Nil(t, apiErr)
	require.NotNil(t, attached)
	assert.Equal(t, "vpc-2", attached.GetVpcId())
	assert.Equal(t, []string{
		"POST /v2/org/test-org/nico/subnet",
		"POST /v2/org/test-org/nico/subnet/subnet-1/attach-vpc",
	}, requests)
}
