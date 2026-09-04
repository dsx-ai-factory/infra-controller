// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// SubnetCreateRequest represents a request to create an IPv4 Subnet.
type SubnetCreateRequest struct {
	Name         string  `json:"name"`
	Description  *string `json:"description"`
	VpcID        string  `json:"vpcId"`
	SubdomainID  *string `json:"subdomainId"`
	IPv4BlockID  string  `json:"ipv4BlockId"`
	PrefixLength int32   `json:"prefixLength"`
}

// SubnetAttachVpcRequest represents a request to attach a Subnet to a VPC.
type SubnetAttachVpcRequest struct {
	VpcID        string `json:"vpcId"`
	AllowReplace bool   `json:"allowReplace"`
}

// SubnetManager manages Subnet operations.
type SubnetManager struct {
	client *Client
}

// NewSubnetManager creates a new SubnetManager.
func NewSubnetManager(client *Client) SubnetManager {
	return SubnetManager{client: client}
}

func toStandardSubnetCreateRequest(request SubnetCreateRequest) standard.SubnetCreateRequest {
	apiRequest := standard.NewSubnetCreateRequest(request.Name, request.VpcID, request.IPv4BlockID, request.PrefixLength)
	if request.Description != nil {
		apiRequest.SetDescription(*request.Description)
	}
	if request.SubdomainID != nil {
		apiRequest.SetSubdomainId(*request.SubdomainID)
	}
	return *apiRequest
}

func toStandardSubnetAttachVpcRequest(request SubnetAttachVpcRequest) standard.SubnetAttachVpcRequest {
	apiRequest := standard.NewSubnetAttachVpcRequest(request.VpcID)
	apiRequest.SetAllowReplace(request.AllowReplace)
	return *apiRequest
}

// Create creates a Subnet.
func (sm SubnetManager) Create(ctx context.Context, request SubnetCreateRequest) (*standard.Subnet, *ApiError) {
	ctx = WithLogger(ctx, sm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, sm.client.Config.Token)

	apiRequest := toStandardSubnetCreateRequest(request)
	apiSubnet, response, err := sm.client.apiClient.SubnetAPI.CreateSubnet(ctx, sm.client.apiMetadata.Organization).
		SubnetCreateRequest(apiRequest).Execute()
	if apiErr := HandleResponseError(response, err); apiErr != nil {
		return nil, apiErr
	}
	return apiSubnet, nil
}

// AttachVpc attaches a Subnet to a VPC, replacing an existing attachment only when allowed.
func (sm SubnetManager) AttachVpc(ctx context.Context, id string, request SubnetAttachVpcRequest) (*standard.Subnet, *ApiError) {
	ctx = WithLogger(ctx, sm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, sm.client.Config.Token)

	apiRequest := toStandardSubnetAttachVpcRequest(request)
	apiSubnet, response, err := sm.client.apiClient.SubnetAPI.AttachVpcToSubnet(ctx, sm.client.apiMetadata.Organization, id).
		SubnetAttachVpcRequest(apiRequest).Execute()
	if apiErr := HandleResponseError(response, err); apiErr != nil {
		return nil, apiErr
	}
	return apiSubnet, nil
}
