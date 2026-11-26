// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// DNSDomain represents a simplified Domain (DNS zone) on a Site.
type DNSDomain struct {
	ID     string `json:"id"`
	SiteID string `json:"siteId"`
	Name   string `json:"name"`
	// DefaultTTL is the default record TTL in seconds. Nil means the Site default.
	DefaultTTL *int32     `json:"defaultTtl"`
	Created    *time.Time `json:"created"`
	Updated    *time.Time `json:"updated"`
}

// DNSDomainCreateRequest is a request to create a Domain on the client's Site.
type DNSDomainCreateRequest struct {
	Name string `json:"name"`
	// DefaultTTL is the default record TTL in seconds. Nil means the Site default.
	DefaultTTL *int32 `json:"defaultTtl"`
}

// DNSDomainUpdateRequest is a request to update a Domain on the client's Site.
type DNSDomainUpdateRequest struct {
	// DefaultTTL is the new default record TTL in seconds.
	DefaultTTL int32 `json:"defaultTtl"`
}

// DNSDomainFilter narrows a Domain list.
type DNSDomainFilter struct {
	// Name returns only Domains with exactly this name.
	Name *string
}

// DNSDomainManager manages Domain operations.
type DNSDomainManager struct {
	client *Client
}

// NewDNSDomainManager creates a new DNSDomainManager.
func NewDNSDomainManager(client *Client) DNSDomainManager {
	return DNSDomainManager{client: client}
}

func dnsDomainFromStandard(api standard.DNSDomain) DNSDomain {
	return DNSDomain{
		ID:         api.Id,
		SiteID:     api.SiteId,
		Name:       api.Name,
		DefaultTTL: api.DefaultTtl.Get(),
		Created:    api.Created.Get(),
		Updated:    api.Updated.Get(),
	}
}

// handleDNSDomainResponseError is HandleResponseError, except that it keeps an
// error that arrives with a success status. The generated client returns one
// when the response body is cut short or cannot be decoded.
func handleDNSDomainResponseError(resp *http.Response, err error) *ApiError {
	if err != nil && resp != nil && resp.StatusCode < http.StatusMultipleChoices {
		return HandleResponseError(nil, err)
	}
	return HandleResponseError(resp, err)
}

// dnsDomainFromResponse converts the result of a call that returns one Domain.
func dnsDomainFromResponse(apiDomain *standard.DNSDomain, resp *http.Response, err error) (*DNSDomain, *ApiError) {
	if apiErr := handleDNSDomainResponseError(resp, err); apiErr != nil {
		return nil, apiErr
	}
	if apiDomain == nil {
		return nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "Domain API returned an empty response",
		}
	}
	domain := dnsDomainFromStandard(*apiDomain)
	return &domain, nil
}

// Create creates a Domain on the client's Site.
func (dm DNSDomainManager) Create(ctx context.Context, request DNSDomainCreateRequest) (*DNSDomain, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiDomain, resp, err := dm.client.apiClient.DNSDomainAPI.CreateDnsDomain(ctx, dm.client.apiMetadata.Organization).
		DNSDomainCreateRequest(standard.DNSDomainCreateRequest{
			SiteId:     dm.client.apiMetadata.SiteID,
			Name:       request.Name,
			DefaultTtl: request.DefaultTTL,
		}).Execute()
	return dnsDomainFromResponse(apiDomain, resp, err)
}

// GetDNSDomains returns the Domains on the client's Site.
func (dm DNSDomainManager) GetDNSDomains(ctx context.Context, domainFilter *DNSDomainFilter, paginationFilter *PaginationFilter) ([]DNSDomain, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	gdr := dm.client.apiClient.DNSDomainAPI.GetAllDnsDomain(ctx, dm.client.apiMetadata.Organization).
		SiteId(dm.client.apiMetadata.SiteID)
	if domainFilter != nil && domainFilter.Name != nil {
		gdr = gdr.Name(*domainFilter.Name)
	}
	if paginationFilter != nil {
		if paginationFilter.PageNumber != nil {
			gdr = gdr.PageNumber(int32(*paginationFilter.PageNumber))
		}
		if paginationFilter.PageSize != nil {
			gdr = gdr.PageSize(int32(*paginationFilter.PageSize))
		}
		if paginationFilter.OrderBy != nil {
			gdr = gdr.OrderBy(*paginationFilter.OrderBy)
		}
	}

	apiDomains, resp, err := gdr.Execute()
	apiErr := handleDNSDomainResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	domains := make([]DNSDomain, 0, len(apiDomains))
	for _, apiDomain := range apiDomains {
		domains = append(domains, dnsDomainFromStandard(apiDomain))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return domains, paginationResponse, nil
}

// Get returns a Domain on the client's Site by ID.
func (dm DNSDomainManager) Get(ctx context.Context, id string) (*DNSDomain, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiDomain, resp, err := dm.client.apiClient.DNSDomainAPI.GetDnsDomain(ctx, dm.client.apiMetadata.Organization, id).
		SiteId(dm.client.apiMetadata.SiteID).Execute()
	return dnsDomainFromResponse(apiDomain, resp, err)
}

// Update changes the default record TTL of a Domain on the client's Site.
func (dm DNSDomainManager) Update(ctx context.Context, id string, request DNSDomainUpdateRequest) (*DNSDomain, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiDomain, resp, err := dm.client.apiClient.DNSDomainAPI.UpdateDnsDomain(ctx, dm.client.apiMetadata.Organization, id).
		DNSDomainUpdateRequest(standard.DNSDomainUpdateRequest{
			SiteId:     dm.client.apiMetadata.SiteID,
			DefaultTtl: request.DefaultTTL,
		}).Execute()
	return dnsDomainFromResponse(apiDomain, resp, err)
}

// Delete deletes a Domain from the client's Site. The API rejects the deletion
// while a Subnet or Machine interface still uses the Domain.
func (dm DNSDomainManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	resp, err := dm.client.apiClient.DNSDomainAPI.DeleteDnsDomain(ctx, dm.client.apiMetadata.Organization, id).
		SiteId(dm.client.apiMetadata.SiteID).Execute()
	return handleDNSDomainResponseError(resp, err)
}
