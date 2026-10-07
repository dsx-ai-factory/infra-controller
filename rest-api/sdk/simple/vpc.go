// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// Vpc represents a simplified VPC
type Vpc struct {
	ID                        string    `json:"id"`
	Name                      string    `json:"name"`
	Description               *string   `json:"description"`
	NetworkVirtualizationType string    `json:"networkVirtualizationType"`
	Created                   time.Time `json:"created"`
	Updated                   time.Time `json:"updated"`
}

// VpcCreateRequest is a request to create a VPC
type VpcCreateRequest struct {
	Name                      string  `json:"name"`
	Description               *string `json:"description"`
	NetworkVirtualizationType string  `json:"networkVirtualizationType"`
}

// VpcUpdateRequest is a request to update a VPC
type VpcUpdateRequest struct {
	Name        *string `json:"name"`
	Description *string `json:"description"`
}

// VpcFilter encapsulates VPC filter parameters
type VpcFilter struct {
	SiteID *string
}

// VpcManager manages VPC operations
type VpcManager struct {
	client *Client
}

// NewVpcManager creates a new VpcManager
func NewVpcManager(client *Client) VpcManager {
	return VpcManager{client: client}
}

func toStandardVpcCreateRequest(request VpcCreateRequest, siteID string) standard.VpcCreateRequest {
	apiReq := *standard.NewVpcCreateRequest(request.Name, siteID)
	if request.Description != nil {
		apiReq.SetDescription(*request.Description)
	}
	if request.NetworkVirtualizationType != "" {
		apiReq.SetNetworkVirtualizationType(request.NetworkVirtualizationType)
	}
	return apiReq
}

func toStandardVpcUpdateRequest(request VpcUpdateRequest) standard.VpcUpdateRequest {
	apiReq := standard.VpcUpdateRequest{}
	if request.Name != nil {
		apiReq.SetName(*request.Name)
	}
	if request.Description != nil {
		apiReq.SetDescription(*request.Description)
	}
	return apiReq
}

func vpcFromStandard(api standard.VPC) Vpc {
	v := Vpc{}
	if api.Id != nil {
		v.ID = *api.Id
	}
	if api.Name != nil {
		v.Name = *api.Name
	}
	v.Description = api.Description.Get()
	if api.NetworkVirtualizationType.IsSet() {
		v.NetworkVirtualizationType = *api.NetworkVirtualizationType.Get()
	}
	if api.Created != nil {
		v.Created = *api.Created
	}
	if api.Updated != nil {
		v.Updated = *api.Updated
	}
	return v
}

// CreateVpc creates a new VPC
func (vm VpcManager) CreateVpc(ctx context.Context, request VpcCreateRequest) (*Vpc, *ApiError) {
	ctx = WithLogger(ctx, vm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, vm.client.Config.Token)

	apiReq := toStandardVpcCreateRequest(request, vm.client.apiMetadata.SiteID)
	apiVpc, resp, err := vm.client.apiClient.VPCAPI.CreateVpc(ctx, vm.client.apiMetadata.Organization).
		VpcCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	v := vpcFromStandard(*apiVpc)
	return &v, nil
}

// GetVpcs returns all VPCs
func (vm VpcManager) GetVpcs(ctx context.Context, vpcFilter *VpcFilter, paginationFilter *PaginationFilter) ([]Vpc, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, vm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, vm.client.Config.Token)

	gvr := vm.client.apiClient.VPCAPI.GetAllVpc(ctx, vm.client.apiMetadata.Organization)
	if vpcFilter != nil && vpcFilter.SiteID != nil {
		gvr = gvr.SiteId(*vpcFilter.SiteID)
	}
	if paginationFilter != nil {
		if paginationFilter.PageNumber != nil {
			gvr = gvr.PageNumber(int32(*paginationFilter.PageNumber))
		}
		if paginationFilter.PageSize != nil {
			gvr = gvr.PageSize(int32(*paginationFilter.PageSize))
		}
		if paginationFilter.OrderBy != nil {
			gvr = gvr.OrderBy(*paginationFilter.OrderBy)
		}
	}

	apiVpcs, resp, err := gvr.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	vpcs := make([]Vpc, 0, len(apiVpcs))
	for _, apiVpc := range apiVpcs {
		vpcs = append(vpcs, vpcFromStandard(apiVpc))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return vpcs, paginationResponse, nil
}

// GetVpc returns a VPC by ID
func (vm VpcManager) GetVpc(ctx context.Context, id string) (*Vpc, *ApiError) {
	ctx = WithLogger(ctx, vm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, vm.client.Config.Token)

	apiVpc, resp, err := vm.client.apiClient.VPCAPI.GetVpc(ctx, vm.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	v := vpcFromStandard(*apiVpc)
	return &v, nil
}

// UpdateVpc updates a VPC
func (vm VpcManager) UpdateVpc(ctx context.Context, id string, request VpcUpdateRequest) (*Vpc, *ApiError) {
	ctx = WithLogger(ctx, vm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, vm.client.Config.Token)

	apiReq := toStandardVpcUpdateRequest(request)
	apiVpc, resp, err := vm.client.apiClient.VPCAPI.UpdateVpc(ctx, vm.client.apiMetadata.Organization, id).
		VpcUpdateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	v := vpcFromStandard(*apiVpc)
	return &v, nil
}

// DeleteVpc deletes a VPC
func (vm VpcManager) DeleteVpc(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, vm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, vm.client.Config.Token)

	_, resp, err := vm.client.apiClient.VPCAPI.DeleteVpc(ctx, vm.client.apiMetadata.Organization, id).Execute()
	return HandleResponseError(resp, err)
}
