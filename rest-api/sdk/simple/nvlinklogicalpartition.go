// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// NVLinkLogicalPartition represents a simplified NVLink Logical Partition
type NVLinkLogicalPartition struct {
	ID          string    `json:"id"`
	Name        string    `json:"name"`
	Description *string   `json:"description"`
	Status      string    `json:"status"`
	Created     time.Time `json:"created"`
	Updated     time.Time `json:"updated"`
}

// NVLinkLogicalPartitionCreateRequest represents a request to create an NVLink Logical Partition
type NVLinkLogicalPartitionCreateRequest struct {
	Name        string  `json:"name"`
	Description *string `json:"description"`
}

// NVLinkLogicalPartitionUpdateRequest represents a request to update an NVLink Logical Partition
type NVLinkLogicalPartitionUpdateRequest struct {
	Name        *string `json:"name"`
	Description *string `json:"description"`
}

// NVLinkLogicalPartitionManager manages NVLink Logical Partition operations
type NVLinkLogicalPartitionManager struct {
	client *Client
}

// NewNVLinkLogicalPartitionManager creates a new NVLinkLogicalPartitionManager
func NewNVLinkLogicalPartitionManager(client *Client) NVLinkLogicalPartitionManager {
	return NVLinkLogicalPartitionManager{client: client}
}

func nvLinkLogicalPartitionFromStandard(api standard.NVLinkLogicalPartition) NVLinkLogicalPartition {
	nl := NVLinkLogicalPartition{}
	if api.Id != nil {
		nl.ID = *api.Id
	}
	if api.Name != nil {
		nl.Name = *api.Name
	}
	nl.Description = api.Description.Get()
	if api.Status != nil {
		nl.Status = string(*api.Status)
	}
	if api.Created != nil {
		nl.Created = *api.Created
	}
	if api.Updated != nil {
		nl.Updated = *api.Updated
	}
	return nl
}

// Create creates a new NVLink Logical Partition
func (nlm NVLinkLogicalPartitionManager) Create(ctx context.Context, request NVLinkLogicalPartitionCreateRequest) (*NVLinkLogicalPartition, *ApiError) {
	ctx = WithLogger(ctx, nlm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, nlm.client.Config.Token)

	apiReq := standard.NVLinkLogicalPartitionCreateRequest{
		Name:   request.Name,
		SiteId: nlm.client.apiMetadata.SiteID,
	}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	apiNl, resp, err := nlm.client.apiClient.NVLinkLogicalPartitionAPI.CreateNvlinkLogicalPartition(ctx, nlm.client.apiMetadata.Organization).
		NVLinkLogicalPartitionCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	nl := nvLinkLogicalPartitionFromStandard(*apiNl)
	return &nl, nil
}

// Get returns an NVLink Logical Partition by ID
func (nlm NVLinkLogicalPartitionManager) Get(ctx context.Context, id string) (*NVLinkLogicalPartition, *ApiError) {
	ctx = WithLogger(ctx, nlm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, nlm.client.Config.Token)

	apiNl, resp, err := nlm.client.apiClient.NVLinkLogicalPartitionAPI.GetNvlinkLogicalPartition(ctx, nlm.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	nl := nvLinkLogicalPartitionFromStandard(*apiNl)
	return &nl, nil
}

// GetNVLinkLogicalPartitions returns all NVLink Logical Partitions
func (nlm NVLinkLogicalPartitionManager) GetNVLinkLogicalPartitions(ctx context.Context, paginationFilter *PaginationFilter) ([]NVLinkLogicalPartition, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, nlm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, nlm.client.Config.Token)

	gir := nlm.client.apiClient.NVLinkLogicalPartitionAPI.GetAllNvlinkLogicalPartition(ctx, nlm.client.apiMetadata.Organization).
		SiteId(nlm.client.apiMetadata.SiteID)
	if paginationFilter != nil {
		if paginationFilter.PageNumber != nil {
			gir = gir.PageNumber(int32(*paginationFilter.PageNumber))
		}
		if paginationFilter.PageSize != nil {
			gir = gir.PageSize(int32(*paginationFilter.PageSize))
		}
		if paginationFilter.OrderBy != nil {
			gir = gir.OrderBy(*paginationFilter.OrderBy)
		}
	}

	apiPartitions, resp, err := gir.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	partitions := make([]NVLinkLogicalPartition, 0, len(apiPartitions))
	for _, p := range apiPartitions {
		partitions = append(partitions, nvLinkLogicalPartitionFromStandard(p))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return partitions, paginationResponse, nil
}

// Update updates an NVLink Logical Partition
func (nlm NVLinkLogicalPartitionManager) Update(ctx context.Context, id string, request NVLinkLogicalPartitionUpdateRequest) (*NVLinkLogicalPartition, *ApiError) {
	ctx = WithLogger(ctx, nlm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, nlm.client.Config.Token)

	apiReq := standard.NVLinkLogicalPartitionUpdateRequest{}
	if request.Name != nil {
		apiReq.Name.Set(request.Name)
	}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	apiNl, resp, err := nlm.client.apiClient.NVLinkLogicalPartitionAPI.UpdateNvlinkLogicalPartition(ctx, nlm.client.apiMetadata.Organization, id).
		NVLinkLogicalPartitionUpdateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	nl := nvLinkLogicalPartitionFromStandard(*apiNl)
	return &nl, nil
}

// Delete deletes an NVLink Logical Partition
func (nlm NVLinkLogicalPartitionManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, nlm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, nlm.client.Config.Token)

	_, resp, err := nlm.client.apiClient.NVLinkLogicalPartitionAPI.DeleteNvlinkLogicalPartition(ctx, nlm.client.apiMetadata.Organization, id).Execute()
	return HandleResponseError(resp, err)
}
