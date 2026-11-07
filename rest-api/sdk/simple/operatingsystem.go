// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// OperatingSystem represents a simplified Operating System
type OperatingSystem struct {
	ID          string    `json:"id"`
	Name        string    `json:"name"`
	Description *string   `json:"description"`
	IpxeScript  *string   `json:"ipxeScript"`
	UserData    *string   `json:"userData"`
	Status      string    `json:"status"`
	Created     time.Time `json:"created"`
	Updated     time.Time `json:"updated"`
}

// OperatingSystemCreateRequest represents a request to create an Operating System
type OperatingSystemCreateRequest struct {
	Name        string  `json:"name"`
	Description *string `json:"description"`
	IpxeScript  *string `json:"ipxeScript"`
	UserData    *string `json:"userData"`
}

// OperatingSystemUpdateRequest represents a request to update an Operating System
type OperatingSystemUpdateRequest struct {
	Name        *string `json:"name"`
	Description *string `json:"description"`
	IpxeScript  *string `json:"ipxeScript"`
	UserData    *string `json:"userData"`
}

// OperatingSystemManager manages Operating System operations
type OperatingSystemManager struct {
	client *Client
}

// NewOperatingSystemManager creates a new OperatingSystemManager
func NewOperatingSystemManager(client *Client) OperatingSystemManager {
	return OperatingSystemManager{client: client}
}

func operatingSystemFromStandard(api standard.OperatingSystem) OperatingSystem {
	os := OperatingSystem{}
	if api.Id != nil {
		os.ID = *api.Id
	}
	if api.Name != nil {
		os.Name = *api.Name
	}
	os.Description = api.Description.Get()
	if api.IpxeScript.IsSet() {
		os.IpxeScript = api.IpxeScript.Get()
	}
	if api.UserData.IsSet() {
		os.UserData = api.UserData.Get()
	}
	if api.Status != nil {
		os.Status = string(*api.Status)
	}
	if api.Created != nil {
		os.Created = *api.Created
	}
	if api.Updated != nil {
		os.Updated = *api.Updated
	}
	return os
}

// Create creates a new Operating System
func (osm OperatingSystemManager) Create(ctx context.Context, request OperatingSystemCreateRequest) (*OperatingSystem, *ApiError) {
	ctx = WithLogger(ctx, osm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, osm.client.Config.Token)

	apiReq := standard.OperatingSystemCreateRequest{Name: request.Name}
	apiReq.Description.Set(request.Description)
	if osm.client.apiMetadata.TenantID != "" {
		apiReq.TenantId.Set(&osm.client.apiMetadata.TenantID)
	}
	if request.IpxeScript != nil {
		apiReq.IpxeScript.Set(request.IpxeScript)
	}
	if request.UserData != nil {
		apiReq.UserData.Set(request.UserData)
	}
	allowOverride := true
	apiReq.AllowOverride = &allowOverride

	apiOS, resp, err := osm.client.apiClient.OperatingSystemAPI.CreateOperatingSystem(ctx, osm.client.apiMetadata.Organization).
		OperatingSystemCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	os := operatingSystemFromStandard(*apiOS)
	return &os, nil
}

// Get returns an Operating System by ID
func (osm OperatingSystemManager) Get(ctx context.Context, id string) (*OperatingSystem, *ApiError) {
	ctx = WithLogger(ctx, osm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, osm.client.Config.Token)

	apiOS, resp, err := osm.client.apiClient.OperatingSystemAPI.GetOperatingSystem(ctx, osm.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	os := operatingSystemFromStandard(*apiOS)
	return &os, nil
}

// GetOperatingSystems returns all Operating Systems
func (osm OperatingSystemManager) GetOperatingSystems(ctx context.Context, paginationFilter *PaginationFilter) ([]OperatingSystem, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, osm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, osm.client.Config.Token)

	gosr := osm.client.apiClient.OperatingSystemAPI.GetAllOperatingSystem(ctx, osm.client.apiMetadata.Organization)
	if paginationFilter != nil {
		if paginationFilter.PageNumber != nil {
			gosr = gosr.PageNumber(int32(*paginationFilter.PageNumber))
		}
		if paginationFilter.PageSize != nil {
			gosr = gosr.PageSize(int32(*paginationFilter.PageSize))
		}
		if paginationFilter.OrderBy != nil {
			gosr = gosr.OrderBy(*paginationFilter.OrderBy)
		}
	}

	apiOss, resp, err := gosr.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	oss := make([]OperatingSystem, 0, len(apiOss))
	for _, o := range apiOss {
		oss = append(oss, operatingSystemFromStandard(o))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return oss, paginationResponse, nil
}

// Update updates an Operating System
func (osm OperatingSystemManager) Update(ctx context.Context, id string, request OperatingSystemUpdateRequest) (*OperatingSystem, *ApiError) {
	ctx = WithLogger(ctx, osm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, osm.client.Config.Token)

	apiReq := standard.OperatingSystemUpdateRequest{}
	if request.Name != nil {
		apiReq.Name.Set(request.Name)
	}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	if request.IpxeScript != nil {
		apiReq.IpxeScript.Set(request.IpxeScript)
	}
	if request.UserData != nil {
		apiReq.UserData.Set(request.UserData)
	}

	apiOS, resp, err := osm.client.apiClient.OperatingSystemAPI.UpdateOperatingSystem(ctx, osm.client.apiMetadata.Organization, id).
		OperatingSystemUpdateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	os := operatingSystemFromStandard(*apiOS)
	return &os, nil
}

// Delete deletes an Operating System
func (osm OperatingSystemManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, osm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, osm.client.Config.Token)

	_, resp, err := osm.client.apiClient.OperatingSystemAPI.DeleteOperatingSystem(ctx, osm.client.apiMetadata.Organization, id).Execute()
	return HandleResponseError(resp, err)
}
