// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/url"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

const domainResponse = `{
	"id":"domain-id",
	"name":"tenant.example.com",
	"siteId":"site-id",
	"tenantId":"tenant-id",
	"created":"2026-09-02T12:00:00Z",
	"updated":"2026-09-02T12:00:00Z"
}`

const subnetResponse = `{
	"id":"subnet-id",
	"name":"tenant-subnet",
	"description":null,
	"siteId":"site-id",
	"vpcId":"target-vpc-id",
	"subdomainId":null,
	"controllerNetworkSegmentId":null,
	"ipv4Prefix":null,
	"ipv4BlockId":null,
	"ipv4Gateway":null,
	"ipv6Prefix":null,
	"ipv6BlockId":null,
	"ipv6Gateway":null,
	"prefixLength":24,
	"routingType":null,
	"status":"Ready",
	"statusHistory":[],
	"created":"2026-09-02T12:00:00Z",
	"updated":"2026-09-02T12:00:00Z"
}`

func TestDomainAPIService_WireContract(t *testing.T) {
	tests := []struct {
		name         string
		method       string
		path         string
		query        url.Values
		body         map[string]any
		status       int
		responseBody string
		execute      func(*APIClient) error
	}{
		{
			name:         "create",
			method:       http.MethodPost,
			path:         "/v2/org/tenant-org/nico/domain",
			query:        url.Values{},
			body:         map[string]any{"name": "tenant.example.com", "siteId": "site-id"},
			status:       http.StatusCreated,
			responseBody: domainResponse,
			execute: func(client *APIClient) error {
				_, _, err := client.DomainAPI.CreateDomain(context.Background(), "tenant-org").
					DomainCreateRequest(*NewDomainCreateRequest("tenant.example.com", "site-id")).
					Execute()
				return err
			},
		},
		{
			name:   "list",
			method: http.MethodGet,
			path:   "/v2/org/tenant-org/nico/domain",
			query: url.Values{
				"siteId":     []string{"site-id"},
				"tenantId":   []string{"tenant-id"},
				"pageNumber": []string{"3"},
				"pageSize":   []string{"25"},
				"orderBy":    []string{"NAME_DESC"},
			},
			status:       http.StatusOK,
			responseBody: "[" + domainResponse + "]",
			execute: func(client *APIClient) error {
				_, _, err := client.DomainAPI.GetAllDomain(context.Background(), "tenant-org").
					SiteId("site-id").
					TenantId("tenant-id").
					PageNumber(3).
					PageSize(25).
					OrderBy("NAME_DESC").
					Execute()
				return err
			},
		},
		{
			name:         "get",
			method:       http.MethodGet,
			path:         "/v2/org/tenant-org/nico/domain/domain-id",
			query:        url.Values{},
			status:       http.StatusOK,
			responseBody: domainResponse,
			execute: func(client *APIClient) error {
				_, _, err := client.DomainAPI.GetDomain(context.Background(), "tenant-org", "domain-id").Execute()
				return err
			},
		},
		{
			name:         "update",
			method:       http.MethodPatch,
			path:         "/v2/org/tenant-org/nico/domain/domain-id",
			query:        url.Values{},
			body:         map[string]any{"name": "renamed.example.com"},
			status:       http.StatusOK,
			responseBody: domainResponse,
			execute: func(client *APIClient) error {
				_, _, err := client.DomainAPI.UpdateDomain(context.Background(), "tenant-org", "domain-id").
					DomainUpdateRequest(*NewDomainUpdateRequest("renamed.example.com")).
					Execute()
				return err
			},
		},
		{
			name:         "delete",
			method:       http.MethodDelete,
			path:         "/v2/org/tenant-org/nico/domain/domain-id",
			query:        url.Values{},
			status:       http.StatusNoContent,
			responseBody: "",
			execute: func(client *APIClient) error {
				_, err := client.DomainAPI.DeleteDomain(context.Background(), "tenant-org", "domain-id").Execute()
				return err
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			transport := &sdkContractTransport{status: tt.status, responseBody: tt.responseBody}
			client := newSDKContractClient(transport)

			require.NoError(t, tt.execute(client))
			require.NotNil(t, transport.request)
			require.Equal(t, tt.method, transport.request.Method)
			require.Equal(t, tt.path, transport.request.URL.Path)
			require.Equal(t, tt.query, transport.request.URL.Query())
			if tt.body == nil {
				require.Empty(t, transport.body)
				return
			}

			var body map[string]any
			require.NoError(t, json.Unmarshal(transport.body, &body))
			require.Equal(t, tt.body, body)
		})
	}
}

func TestDomain_MarshalJSON(t *testing.T) {
	encoded, err := json.Marshal(Domain{})
	require.NoError(t, err)

	var response map[string]any
	require.NoError(t, json.Unmarshal(encoded, &response))
	require.Len(t, response, 6)
	for _, field := range []string{"id", "name", "siteId", "tenantId", "created", "updated"} {
		require.Contains(t, response, field)
	}
	require.NotContains(t, response, "controllerDomainId")
}

func TestSubnetAPIService_AttachVpcToSubnet(t *testing.T) {
	transport := &sdkContractTransport{status: http.StatusOK, responseBody: subnetResponse}
	client := newSDKContractClient(transport)

	subnet, _, err := client.SubnetAPI.AttachVpcToSubnet(context.Background(), "tenant-org", "subnet-id").
		SubnetAttachVpcRequest(*NewSubnetAttachVpcRequest("target-vpc-id")).
		Execute()
	require.NoError(t, err)
	require.Equal(t, "target-vpc-id", subnet.GetVpcId())
	require.NotNil(t, transport.request)
	require.Equal(t, http.MethodPost, transport.request.Method)
	require.Equal(t, "/v2/org/tenant-org/nico/subnet/subnet-id/attach-vpc", transport.request.URL.Path)

	var body map[string]any
	require.NoError(t, json.Unmarshal(transport.body, &body))
	require.Equal(t, map[string]any{"vpcId": "target-vpc-id", "allowReplace": false}, body)
}

func TestSubnetCreateRequest_MarshalJSON(t *testing.T) {
	subdomainID := "domain-id"
	tests := []struct {
		name        string
		subdomainID *string
	}{
		{name: "omitted"},
		{name: "set", subdomainID: &subdomainID},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			request := NewSubnetCreateRequest("tenant-subnet", "vpc-id", "ip-block-id", 24)
			if tt.subdomainID != nil {
				request.SetSubdomainId(*tt.subdomainID)
			}

			encoded, err := json.Marshal(request)
			require.NoError(t, err)

			var body map[string]any
			require.NoError(t, json.Unmarshal(encoded, &body))
			if tt.subdomainID == nil {
				require.NotContains(t, body, "subdomainId")
				return
			}
			require.Equal(t, *tt.subdomainID, body["subdomainId"])
		})
	}
}

func TestSubnet_UnmarshalJSON(t *testing.T) {
	t.Run("accepts and preserves explicit null subdomain", func(t *testing.T) {
		var subnet Subnet
		require.NoError(t, json.Unmarshal([]byte(subnetResponse), &subnet))

		subdomainID, set := subnet.GetSubdomainIdOk()
		require.True(t, set)
		require.Nil(t, subdomainID)

		encoded, err := json.Marshal(subnet)
		require.NoError(t, err)

		var response map[string]any
		require.NoError(t, json.Unmarshal(encoded, &response))
		require.Contains(t, response, "subdomainId")
		require.Nil(t, response["subdomainId"])
	})

	t.Run("rejects missing required subdomain", func(t *testing.T) {
		var response map[string]any
		require.NoError(t, json.Unmarshal([]byte(subnetResponse), &response))
		delete(response, "subdomainId")

		missingSubdomain, err := json.Marshal(response)
		require.NoError(t, err)

		var subnet Subnet
		require.ErrorContains(t, json.Unmarshal(missingSubdomain, &subnet), "required property subdomainId")
	})
}

type sdkContractTransport struct {
	request      *http.Request
	body         []byte
	status       int
	responseBody string
}

func (t *sdkContractTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	t.request = request.Clone(request.Context())
	if request.Body != nil {
		body, err := io.ReadAll(request.Body)
		if err != nil {
			return nil, err
		}
		t.body = body
	}

	return &http.Response{
		StatusCode: t.status,
		Header:     http.Header{"Content-Type": []string{"application/json"}},
		Body:       io.NopCloser(strings.NewReader(t.responseBody)),
		Request:    request,
	}, nil
}

func newSDKContractClient(transport *sdkContractTransport) *APIClient {
	configuration := NewConfiguration()
	configuration.Servers = ServerConfigurations{{URL: "https://example.com", Description: "test"}}
	configuration.HTTPClient = &http.Client{Transport: transport}
	return NewAPIClient(configuration)
}
