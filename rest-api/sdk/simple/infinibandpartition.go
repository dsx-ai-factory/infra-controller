// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// InfinibandPartition represents a simplified InfiniBand Partition
type InfinibandPartition struct {
	ID           string    `json:"id"`
	Name         string    `json:"name"`
	Description  *string   `json:"description"`
	PartitionKey *string   `json:"partitionKey"`
	Created      time.Time `json:"created"`
	Updated      time.Time `json:"updated"`
	Status       string    `json:"status"`
}

// InfinibandPartitionCreateRequest represents a request to create an InfiniBand Partition
type InfinibandPartitionCreateRequest struct {
	Name        string  `json:"name"`
	Description *string `json:"description"`
}

// InfinibandPartitionUpdateRequest represents a request to update an InfiniBand Partition
type InfinibandPartitionUpdateRequest struct {
	Name        *string `json:"name"`
	Description *string `json:"description"`
}

// InfinibandPartitionManager manages InfiniBand Partition operations
type InfinibandPartitionManager struct {
	client *Client
}

// NewInfinibandPartitionManager creates a new InfinibandPartitionManager
func NewInfinibandPartitionManager(client *Client) InfinibandPartitionManager {
	return InfinibandPartitionManager{client: client}
}

func infinibandPartitionFromStandard(api standard.InfiniBandPartition) InfinibandPartition {
	ip := InfinibandPartition{}
	if api.Id != nil {
		ip.ID = *api.Id
	}
	if api.Name != nil {
		ip.Name = *api.Name
	}
	ip.Description = api.Description.Get()
	if api.PartitionKey.IsSet() {
		ip.PartitionKey = api.PartitionKey.Get()
	}
	if api.Created != nil {
		ip.Created = *api.Created
	}
	if api.Updated != nil {
		ip.Updated = *api.Updated
	}
	if api.Status != nil {
		ip.Status = string(*api.Status)
	}
	return ip
}

// Create creates a new InfiniBand Partition
func (ipm InfinibandPartitionManager) Create(ctx context.Context, request InfinibandPartitionCreateRequest) (*InfinibandPartition, *ApiError) {
	ctx = WithLogger(ctx, ipm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, ipm.client.Config.Token)

	apiReq := standard.InfiniBandPartitionCreateRequest{
		Name:   request.Name,
		SiteId: ipm.client.apiMetadata.SiteID,
	}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	apiIb, resp, err := ipm.client.apiClient.InfiniBandPartitionAPI.CreateInfinibandPartition(ctx, ipm.client.apiMetadata.Organization).
		InfiniBandPartitionCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	ib := infinibandPartitionFromStandard(*apiIb)
	return &ib, nil
}

// Get returns an InfiniBand Partition by ID
func (ipm InfinibandPartitionManager) Get(ctx context.Context, id string) (*InfinibandPartition, *ApiError) {
	ctx = WithLogger(ctx, ipm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, ipm.client.Config.Token)

	apiIb, resp, err := ipm.client.apiClient.InfiniBandPartitionAPI.GetInfinibandPartition(ctx, ipm.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	ib := infinibandPartitionFromStandard(*apiIb)
	return &ib, nil
}

// GetInfinibandPartitions returns all InfiniBand Partitions
func (ipm InfinibandPartitionManager) GetInfinibandPartitions(ctx context.Context, paginationFilter *PaginationFilter) ([]InfinibandPartition, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, ipm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, ipm.client.Config.Token)

	gir := ipm.client.apiClient.InfiniBandPartitionAPI.GetAllInfinibandPartition(ctx, ipm.client.apiMetadata.Organization).
		SiteId(ipm.client.apiMetadata.SiteID)
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

	partitions := make([]InfinibandPartition, 0, len(apiPartitions))
	for _, p := range apiPartitions {
		partitions = append(partitions, infinibandPartitionFromStandard(p))
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

// Update updates an InfiniBand Partition
func (ipm InfinibandPartitionManager) Update(ctx context.Context, id string, request InfinibandPartitionUpdateRequest) (*InfinibandPartition, *ApiError) {
	ctx = WithLogger(ctx, ipm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, ipm.client.Config.Token)

	existing, apiErr := ipm.Get(ctx, id)
	if apiErr != nil {
		return nil, apiErr
	}
	name := existing.Name
	if request.Name != nil {
		name = *request.Name
	}
	apiReq := standard.InfiniBandPartitionUpdateRequest{Name: name}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	apiIb, resp, err := ipm.client.apiClient.InfiniBandPartitionAPI.UpdateInfinibandPartition(ctx, ipm.client.apiMetadata.Organization, id).
		InfiniBandPartitionUpdateRequest(apiReq).Execute()
	apiErr = HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	ib := infinibandPartitionFromStandard(*apiIb)
	return &ib, nil
}

// Delete deletes an InfiniBand Partition
func (ipm InfinibandPartitionManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, ipm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, ipm.client.Config.Token)

	_, resp, err := ipm.client.apiClient.InfiniBandPartitionAPI.DeleteInfinibandPartition(ctx, ipm.client.apiMetadata.Organization, id).Execute()
	return HandleResponseError(resp, err)
}
