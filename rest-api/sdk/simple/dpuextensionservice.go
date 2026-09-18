// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

const (
	// DpuExtensionServiceTypeKubernetesPod is the type of the DPU extension service for Kubernetes Pod
	DpuExtensionServiceTypeKubernetesPod = "KubernetesPod"
)

// DpuExtensionService represents a simplified DPU Extension Service
type DpuExtensionService struct {
	ID             string    `json:"id"`
	Name           string    `json:"name"`
	Description    *string   `json:"description"`
	ServiceType    string    `json:"serviceType"`
	Version        *string   `json:"version"`
	ActiveVersions []string  `json:"activeVersions"`
	Status         string    `json:"status"`
	Created        time.Time `json:"created"`
	Updated        time.Time `json:"updated"`
}

// DpuExtensionServiceCreateRequest represents a request to create a DPU Extension Service
type DpuExtensionServiceCreateRequest struct {
	Name        string  `json:"name"`
	Description *string `json:"description"`
	ServiceType string  `json:"serviceType"`
	Data        string  `json:"data"`
}

// DpuExtensionServiceUpdateRequest represents a request to update a DPU Extension Service
type DpuExtensionServiceUpdateRequest struct {
	Name        *string `json:"name"`
	Description *string `json:"description"`
	Data        *string `json:"data"`
}

// DpuExtensionServiceManager manages DPU Extension Service operations
type DpuExtensionServiceManager struct {
	client *Client
}

// NewDpuExtensionServiceManager creates a new DpuExtensionServiceManager
func NewDpuExtensionServiceManager(client *Client) DpuExtensionServiceManager {
	return DpuExtensionServiceManager{client: client}
}

func dpuExtensionServiceFromStandard(api standard.DpuExtensionService) DpuExtensionService {
	des := DpuExtensionService{
		ActiveVersions: api.ActiveVersions,
	}
	if api.Id != nil {
		des.ID = *api.Id
	}
	if api.Name != nil {
		des.Name = *api.Name
	}
	if api.Description.IsSet() {
		des.Description = api.Description.Get()
	}
	if api.ServiceType != nil {
		des.ServiceType = *api.ServiceType
	}
	if api.Version.IsSet() {
		des.Version = api.Version.Get()
	}
	if api.Status != nil {
		des.Status = string(*api.Status)
	}
	if api.Created != nil {
		des.Created = *api.Created
	}
	if api.Updated != nil {
		des.Updated = *api.Updated
	}
	return des
}

// Create creates a new DPU Extension Service
func (dm DpuExtensionServiceManager) Create(ctx context.Context, request DpuExtensionServiceCreateRequest) (*DpuExtensionService, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiReq := standard.DpuExtensionServiceCreateRequest{
		Name:        request.Name,
		ServiceType: request.ServiceType,
		SiteId:      dm.client.apiMetadata.SiteID,
		Data:        request.Data,
	}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	apiDes, resp, err := dm.client.apiClient.DPUExtensionServiceAPI.CreateDpuExtensionService(ctx, dm.client.apiMetadata.Organization).
		DpuExtensionServiceCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	des := dpuExtensionServiceFromStandard(*apiDes)
	return &des, nil
}

// Get returns a DPU Extension Service by ID
func (dm DpuExtensionServiceManager) Get(ctx context.Context, id string) (*DpuExtensionService, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiDes, resp, err := dm.client.apiClient.DPUExtensionServiceAPI.GetDpuExtensionService(ctx, dm.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	des := dpuExtensionServiceFromStandard(*apiDes)
	return &des, nil
}

// GetDpuExtensionServices returns all DPU Extension Services
func (dm DpuExtensionServiceManager) GetDpuExtensionServices(ctx context.Context, paginationFilter *PaginationFilter) ([]DpuExtensionService, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	gir := dm.client.apiClient.DPUExtensionServiceAPI.GetAllDpuExtensionService(ctx, dm.client.apiMetadata.Organization).
		SiteId(dm.client.apiMetadata.SiteID)
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

	apiServices, resp, err := gir.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	services := make([]DpuExtensionService, 0, len(apiServices))
	for _, s := range apiServices {
		services = append(services, dpuExtensionServiceFromStandard(s))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return services, paginationResponse, nil
}

// Update updates a DPU Extension Service
func (dm DpuExtensionServiceManager) Update(ctx context.Context, id string, request DpuExtensionServiceUpdateRequest) (*DpuExtensionService, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiReq := standard.DpuExtensionServiceUpdateRequest{}
	if request.Name != nil {
		apiReq.Name.Set(request.Name)
	}
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	if request.Data != nil {
		apiReq.Data.Set(request.Data)
	}
	apiDes, resp, err := dm.client.apiClient.DPUExtensionServiceAPI.UpdateDpuExtensionService(ctx, dm.client.apiMetadata.Organization, id).
		DpuExtensionServiceUpdateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	des := dpuExtensionServiceFromStandard(*apiDes)
	return &des, nil
}

// Delete deletes a DPU Extension Service
func (dm DpuExtensionServiceManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	resp, err := dm.client.apiClient.DPUExtensionServiceAPI.DeleteDpuExtensionService(ctx, dm.client.apiMetadata.Organization, id).Execute()
	return HandleResponseError(resp, err)
}

// GetDpuExtensionServiceVersion returns information about a specific version of a DPU extension service
func (dm DpuExtensionServiceManager) GetDpuExtensionServiceVersion(ctx context.Context, id string, version string) (*standard.DpuExtensionServiceVersionInfo, *ApiError) {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	apiInfo, resp, err := dm.client.apiClient.DPUExtensionServiceAPI.GetDpuExtensionServiceVersion(ctx, dm.client.apiMetadata.Organization, id, version).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return apiInfo, nil
}

// DeleteDpuExtensionServiceVersion deletes a specific version of a DPU extension service
func (dm DpuExtensionServiceManager) DeleteDpuExtensionServiceVersion(ctx context.Context, id string, version string) *ApiError {
	ctx = WithLogger(ctx, dm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, dm.client.Config.Token)

	resp, err := dm.client.apiClient.DPUExtensionServiceAPI.DeleteDpuExtensionServiceVersion(ctx, dm.client.apiMetadata.Organization, id, version).Execute()
	return HandleResponseError(resp, err)
}
