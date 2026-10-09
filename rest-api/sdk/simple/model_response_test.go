// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const (
	validDomainResponseJSON = `{"id":"domain-1","name":"tenant.example.com","siteId":"site-1","tenantId":"tenant-1","status":"Ready","created":"2026-09-02T12:00:00Z","updated":"2026-09-02T12:00:00Z"}`
	validSubnetResponseJSON = `{"id":"subnet-1","name":"tenant-net","description":null,"siteId":"site-1","vpcId":"vpc-1","subdomainId":"domain-1","controllerNetworkSegmentId":"controller-subnet-1","ipv4Prefix":"10.0.0.0","ipv4BlockId":"block-1","ipv4Gateway":"10.0.0.1","ipv6Prefix":null,"ipv6BlockId":null,"ipv6Gateway":null,"prefixLength":24,"routingType":"Public","status":"Ready","statusHistory":[],"created":"2026-09-02T00:00:00Z","updated":"2026-09-02T00:00:00Z"}`
	// Every newly required Subnet response property except subdomainId.
	subnetWithoutSubdomainJSON = `{"id":"subnet-1","name":"tenant-net","description":null,"siteId":"site-1","vpcId":"vpc-1","controllerNetworkSegmentId":"controller-subnet-1","ipv4Prefix":"10.0.0.0","ipv4BlockId":"block-1","ipv4Gateway":"10.0.0.1","ipv6Prefix":null,"ipv6BlockId":null,"ipv6Gateway":null,"prefixLength":24,"routingType":"Public","status":"Ready","statusHistory":[],"created":"2026-09-02T00:00:00Z","updated":"2026-09-02T00:00:00Z"}`
)

// invalidModelBodies are 2xx response bodies that do not satisfy the generated
// response model and therefore must be reported as an invalid API response.
var invalidModelBodies = map[string]string{
	"truncated JSON": `{`,
	"JSON null":      `null`,
	"empty object":   `{}`,
}

func serveStatusBody(t *testing.T, status int, body string) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}))
	t.Cleanup(server.Close)
	return server
}

func requireInvalidResponse(t *testing.T, apiErr *ApiError) {
	t.Helper()
	require.NotNil(t, apiErr, "an invalid 2xx response model must not be reported as success")
	assert.Equal(t, http.StatusBadGateway, apiErr.Code)
}

func TestClientDomainOperations_RejectInvalidSuccessResponses(t *testing.T) {
	ctx := context.Background()
	for name, body := range invalidModelBodies {
		t.Run("CreateDomain "+name, func(t *testing.T) {
			client := newSimpleTestClient(serveStatusBody(t, http.StatusCreated, body).URL)
			var domain *Domain
			var apiErr *ApiError
			require.NotPanics(t, func() {
				domain, apiErr = client.CreateDomain(ctx, DomainCreateRequest{Name: "tenant.example.com"})
			})
			assert.Nil(t, domain)
			requireInvalidResponse(t, apiErr)
		})
		t.Run("GetDomain "+name, func(t *testing.T) {
			client := newSimpleTestClient(serveStatusBody(t, http.StatusOK, body).URL)
			var domain *Domain
			var apiErr *ApiError
			require.NotPanics(t, func() {
				domain, apiErr = client.GetDomain(ctx, "domain-1")
			})
			assert.Nil(t, domain)
			requireInvalidResponse(t, apiErr)
		})
	}

	for name, body := range map[string]string{
		"truncated JSON":   `[`,
		"JSON null":        `null`,
		"object not array": `{}`,
		"invalid element":  `[{}]`,
	} {
		t.Run("GetDomains "+name, func(t *testing.T) {
			client := newSimpleTestClient(serveStatusBody(t, http.StatusOK, body).URL)
			domains, apiErr := client.GetDomains(ctx, nil)
			assert.Nil(t, domains)
			requireInvalidResponse(t, apiErr)
		})
	}

	t.Run("GetDomains empty array is a valid empty result", func(t *testing.T) {
		client := newSimpleTestClient(serveStatusBody(t, http.StatusOK, `[]`).URL)
		domains, apiErr := client.GetDomains(ctx, nil)
		require.Nil(t, apiErr)
		assert.NotNil(t, domains)
		assert.Empty(t, domains)
	})

	t.Run("valid Domain responses succeed", func(t *testing.T) {
		created, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusCreated, validDomainResponseJSON).URL).
			CreateDomain(ctx, DomainCreateRequest{Name: "tenant.example.com"})
		require.Nil(t, apiErr)
		require.NotNil(t, created)
		assert.Equal(t, "domain-1", created.ID)

		domain, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusOK, validDomainResponseJSON).URL).
			GetDomain(ctx, "domain-1")
		require.Nil(t, apiErr)
		require.NotNil(t, domain)
		assert.Equal(t, "domain-1", domain.ID)

		domains, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusOK, "["+validDomainResponseJSON+"]").URL).
			GetDomains(ctx, nil)
		require.Nil(t, apiErr)
		require.Len(t, domains, 1)
		assert.Equal(t, "domain-1", domains[0].ID)
	})

	t.Run("API error status keeps its code", func(t *testing.T) {
		client := newSimpleTestClient(serveStatusBody(t, http.StatusNotFound, `{"message":"Domain not found"}`).URL)
		domain, apiErr := client.GetDomain(ctx, "domain-1")
		assert.Nil(t, domain)
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusNotFound, apiErr.Code)
	})
}

func TestClientSubnetOperations_RejectInvalidSuccessResponses(t *testing.T) {
	ctx := context.Background()
	bodies := map[string]string{"missing required subdomainId": subnetWithoutSubdomainJSON}
	for name, body := range invalidModelBodies {
		bodies[name] = body
	}
	domainID := "domain-1"
	createRequest := SubnetCreateRequest{Name: "tenant-net", VpcID: "vpc-1", SubdomainID: &domainID, IPv4BlockID: "block-1", PrefixLength: 24}
	attachRequest := SubnetAttachVpcRequest{VpcID: "vpc-2", AllowReplace: true}

	for name, body := range bodies {
		t.Run("CreateSubnet "+name, func(t *testing.T) {
			subnet, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusCreated, body).URL).CreateSubnet(ctx, createRequest)
			assert.Nil(t, subnet)
			requireInvalidResponse(t, apiErr)
		})
		t.Run("AttachSubnetToVpc "+name, func(t *testing.T) {
			subnet, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusOK, body).URL).AttachSubnetToVpc(ctx, "subnet-1", attachRequest)
			assert.Nil(t, subnet)
			requireInvalidResponse(t, apiErr)
		})
	}

	t.Run("valid Subnet responses succeed", func(t *testing.T) {
		created, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusCreated, validSubnetResponseJSON).URL).CreateSubnet(ctx, createRequest)
		require.Nil(t, apiErr)
		require.NotNil(t, created)
		assert.Equal(t, "subnet-1", created.GetId())
		assert.Equal(t, "domain-1", created.GetSubdomainId())

		attached, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusOK, validSubnetResponseJSON).URL).AttachSubnetToVpc(ctx, "subnet-1", attachRequest)
		require.Nil(t, apiErr)
		require.NotNil(t, attached)
		assert.Equal(t, "subnet-1", attached.GetId())
	})

	t.Run("API conflict status keeps its code", func(t *testing.T) {
		subnet, apiErr := newSimpleTestClient(serveStatusBody(t, http.StatusConflict, `{"message":"Subnet is attached"}`).URL).AttachSubnetToVpc(ctx, "subnet-1", attachRequest)
		assert.Nil(t, subnet)
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusConflict, apiErr.Code)
		assert.Equal(t, "Subnet is attached", apiErr.Message)
	})
}
