// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"fmt"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// IpBlock represents a simplified IP block
type IpBlock struct {
	ID              string    `json:"id"`
	Name            *string   `json:"name"`
	Description     *string   `json:"description"`
	SiteID          *string   `json:"siteId"`
	Cidr            string    `json:"cidr"` // prefix/prefixLength
	ProtocolVersion *string   `json:"protocolVersion"`
	Status          string    `json:"status"`
	Created         time.Time `json:"created"`
	Updated         time.Time `json:"updated"`
}

// IpBlockManager manages IP block operations
type IpBlockManager struct {
	client *Client
}

// NewIpBlockManager creates a new IpBlockManager
func NewIpBlockManager(client *Client) IpBlockManager {
	return IpBlockManager{client: client}
}

func ipBlockFromStandard(api standard.IpBlock) IpBlock {
	ib := IpBlock{
		Name:            api.Name,
		Description:     api.Description.Get(),
		SiteID:          api.SiteId,
		ProtocolVersion: api.ProtocolVersion,
	}
	if api.Id != nil {
		ib.ID = *api.Id
	}
	if api.Prefix != nil && api.PrefixLength != nil {
		ib.Cidr = fmt.Sprintf("%s/%d", *api.Prefix, *api.PrefixLength)
	}
	if api.Status != nil {
		ib.Status = string(*api.Status)
	}
	if api.Created != nil {
		ib.Created = *api.Created
	}
	if api.Updated != nil {
		ib.Updated = *api.Updated
	}
	return ib
}

// GetIpBlocks returns all IP blocks
func (im IpBlockManager) GetIpBlocks(ctx context.Context, paginationFilter *PaginationFilter) ([]IpBlock, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)

	gmr := im.client.apiClient.IPBlockAPI.GetAllIpblock(ctx, im.client.apiMetadata.Organization)
	if im.client.apiMetadata.SiteID != "" {
		gmr = gmr.SiteId(im.client.apiMetadata.SiteID)
	}
	if paginationFilter != nil {
		if paginationFilter.PageNumber != nil {
			gmr = gmr.PageNumber(int32(*paginationFilter.PageNumber))
		}
		if paginationFilter.PageSize != nil {
			gmr = gmr.PageSize(int32(*paginationFilter.PageSize))
		}
		if paginationFilter.OrderBy != nil {
			gmr = gmr.OrderBy(*paginationFilter.OrderBy)
		}
	}

	apiBlocks, resp, err := gmr.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	blocks := make([]IpBlock, 0, len(apiBlocks))
	for _, b := range apiBlocks {
		blocks = append(blocks, ipBlockFromStandard(b))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return blocks, paginationResponse, nil
}

// GetIpBlock returns an IP block by ID
func (im IpBlockManager) GetIpBlock(ctx context.Context, id string) (*IpBlock, *ApiError) {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)

	apiBlock, resp, err := im.client.apiClient.IPBlockAPI.GetIpblock(ctx, im.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	ib := ipBlockFromStandard(*apiBlock)
	return &ib, nil
}
