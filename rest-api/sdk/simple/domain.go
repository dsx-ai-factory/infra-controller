// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// Domain represents a tenant-owned DNS zone at a Site.
type Domain struct {
	Status   string    `json:"status"`
	ID       string    `json:"id"`
	Name     string    `json:"name"`
	SiteID   string    `json:"siteId"`
	TenantID string    `json:"tenantId"`
	Created  time.Time `json:"created"`
	Updated  time.Time `json:"updated"`
}

// DomainCreateRequest represents a request to create a Domain at the client's selected Site.
type DomainCreateRequest struct {
	Name string `json:"name"`
}

// DomainFilter encapsulates Domain filter parameters.
type DomainFilter struct {
	SiteID   *string
	TenantID *string
}

// DomainManager manages Domain operations.
type DomainManager struct {
	client *Client
}

// NewDomainManager creates a new DomainManager.
func NewDomainManager(client *Client) DomainManager {
	return DomainManager{client: client}
}

func domainFromStandard(api standard.Domain) Domain {
	return Domain{
		Status:   api.Status,
		ID:       api.Id,
		Name:     api.Name,
		SiteID:   api.SiteId,
		TenantID: api.TenantId,
		Created:  api.Created,
		Updated:  api.Updated,
	}
}

// Create creates a Domain at the client's selected Site.
func (dm DomainManager) Create(ctx context.Context, request DomainCreateRequest) (*Domain, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiRequest := standard.NewDomainCreateRequest(request.Name, dm.client.apiMetadata.SiteID)
	apiDomain, response, err := dm.client.apiClient.DNSDomainAPI.CreateDomain(ctx, dm.client.apiMetadata.Organization).
		DomainCreateRequest(*apiRequest).Execute()
	if apiErr := domainResponseError(response, err); apiErr != nil {
		return nil, apiErr
	}
	domain := domainFromStandard(*apiDomain)
	return &domain, nil
}

// GetDomains returns tenant-owned Domains, optionally filtered by Site.
func (dm DomainManager) GetDomains(ctx context.Context, domainFilter *DomainFilter) ([]Domain, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	request := dm.client.apiClient.DNSDomainAPI.GetAllDomain(ctx, dm.client.apiMetadata.Organization)
	if domainFilter != nil && domainFilter.SiteID != nil {
		request = request.SiteId(*domainFilter.SiteID)
	}
	if domainFilter != nil && domainFilter.TenantID != nil {
		request = request.TenantId(*domainFilter.TenantID)
	}
	apiDomains, response, err := request.Execute()
	if apiErr := domainResponseError(response, err); apiErr != nil {
		return nil, apiErr
	}

	domains := make([]Domain, 0, len(apiDomains))
	for _, apiDomain := range apiDomains {
		domains = append(domains, domainFromStandard(apiDomain))
	}
	return domains, nil
}

// Get returns a Domain by ID.
func (dm DomainManager) Get(ctx context.Context, id string) (*Domain, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiDomain, response, err := dm.client.apiClient.DNSDomainAPI.GetDomain(ctx, dm.client.apiMetadata.Organization, id).Execute()
	if apiErr := domainResponseError(response, err); apiErr != nil {
		return nil, apiErr
	}
	domain := domainFromStandard(*apiDomain)
	return &domain, nil
}

// Delete deletes a Domain by ID.
func (dm DomainManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	response, err := dm.client.apiClient.DNSDomainAPI.DeleteDomain(ctx, dm.client.apiMetadata.Organization, id).Execute()
	return domainResponseError(response, err)
}

// A successful HTTP status with an invalid generated response model must not
// become a successful mutation with zero-valued Domain identity.
func domainResponseError(response *http.Response, err error) *ApiError {
	if response != nil && response.StatusCode < http.StatusMultipleChoices && err != nil {
		return &ApiError{Code: http.StatusBadGateway, Message: "Invalid Domain response from API"}
	}
	return HandleResponseError(response, err)
}
