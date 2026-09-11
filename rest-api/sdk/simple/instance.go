// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// InstanceCreateRequest represents a simplified request to create an Instance
type InstanceCreateRequest struct {
	Name                           string                                     `json:"name"`
	Description                    *string                                    `json:"description"`
	MachineID                      string                                     `json:"machineId"`
	VpcID                          *string                                    `json:"vpcId"`
	IpxeScript                     string                                     `json:"ipxeScript"`
	UserData                       *string                                    `json:"userData"`
	SSHKeys                        []string                                   `json:"sshKeys"`
	Labels                         map[string]string                          `json:"labels"`
	InfinibandInterfaces           []InfiniBandInterfaceCreateOrUpdateRequest `json:"infinibandInterfaces"`
	NVLinkInterfaces               []NVLinkInterfaceCreateOrUpdateRequest     `json:"nvLinkInterfaces"`
	DpuExtensionServiceDeployments []DpuExtensionServiceDeploymentRequest     `json:"dpuExtensionServiceDeployments"`
}

// InfiniBandInterfaceCreateOrUpdateRequest represents an InfiniBand interface attachment
type InfiniBandInterfaceCreateOrUpdateRequest struct {
	PartitionID       string  `json:"partitionId"`
	Device            string  `json:"device"`
	Vendor            *string `json:"vendor"`
	DeviceInstance    int     `json:"deviceInstance"`
	IsPhysical        bool    `json:"isPhysical"`
	VirtualFunctionID *int    `json:"virtualFunctionId"`
}

// NVLinkInterfaceCreateOrUpdateRequest represents an NVLink interface attachment
type NVLinkInterfaceCreateOrUpdateRequest struct {
	NVLinkLogicalPartitionID string `json:"nvLinkLogicalPartitionId"`
	DeviceInstance           int    `json:"deviceInstance"`
}

// DpuExtensionServiceDeploymentRequest represents a DPU extension service deployment
type DpuExtensionServiceDeploymentRequest struct {
	DpuExtensionServiceID string  `json:"dpuExtensionServiceId"`
	Version               *string `json:"version"`
}

// InstanceFilter encapsulates instance list filter parameters
type InstanceFilter struct {
	Name      *string
	Query     *string
	VpcID     *string
	IPAddress *string
}

// InstanceUpdateRequest represents a simplified request to update an Instance
type InstanceUpdateRequest struct {
	Name                           *string                                    `json:"name"`
	Description                    *string                                    `json:"description"`
	IpxeScript                     *string                                    `json:"ipxeScript"`
	UserData                       *string                                    `json:"userData"`
	Labels                         map[string]string                          `json:"labels"`
	InfinibandInterfaces           []InfiniBandInterfaceCreateOrUpdateRequest `json:"infinibandInterfaces"`
	NVLinkInterfaces               []NVLinkInterfaceCreateOrUpdateRequest     `json:"nvLinkInterfaces"`
	DpuExtensionServiceDeployments []DpuExtensionServiceDeploymentRequest     `json:"dpuExtensionServiceDeployments"`
}

// InstanceManager manages Instance operations
type InstanceManager struct {
	client *Client
}

// NewInstanceManager creates a new InstanceManager
func NewInstanceManager(client *Client) InstanceManager {
	return InstanceManager{client: client}
}

func toStandardInfiniBandInterface(request InfiniBandInterfaceCreateOrUpdateRequest) standard.InfiniBandInterfaceCreateRequest {
	apiReq := standard.InfiniBandInterfaceCreateRequest{
		PartitionId:    &request.PartitionID,
		Device:         &request.Device,
		DeviceInstance: standard.PtrInt32(int32(request.DeviceInstance)),
		IsPhysical:     standard.PtrBool(request.IsPhysical),
	}
	if request.Vendor != nil {
		apiReq.Vendor.Set(request.Vendor)
	}
	if request.VirtualFunctionID != nil {
		vf := int32(*request.VirtualFunctionID)
		apiReq.VirtualFunctionId.Set(&vf)
	}
	return apiReq
}

func toStandardNVLinkInterface(request NVLinkInterfaceCreateOrUpdateRequest) standard.NVLinkInterfaceCreateOrUpdateRequest {
	return standard.NVLinkInterfaceCreateOrUpdateRequest{
		NvLinkLogicalPartitionId: &request.NVLinkLogicalPartitionID,
		DeviceInstance:           standard.PtrInt32(int32(request.DeviceInstance)),
	}
}

func toStandardDpuExtensionDeployment(request DpuExtensionServiceDeploymentRequest) standard.DpuExtensionServiceDeploymentRequest {
	apiReq := standard.DpuExtensionServiceDeploymentRequest{
		DpuExtensionServiceId: standard.PtrString(request.DpuExtensionServiceID),
	}
	if request.Version != nil {
		apiReq.Version = request.Version
	}
	return apiReq
}

func toStandardInstanceCreateRequest(request InstanceCreateRequest, sshKeyGroupIDs []string, am ApiMetadata) standard.InstanceCreateRequest {
	vpcID := am.VpcID
	useDefaultVpcMetadata := true
	if request.VpcID != nil {
		vpcID = *request.VpcID
		// Only use ApiMetadata-derived prefix/subnet when the effective VPC
		// matches the metadata VPC; otherwise, let the backend choose
		// appropriate defaults for the overridden VPC.
		if *request.VpcID != am.VpcID {
			useDefaultVpcMetadata = false
		}
	}
	defaultIface := standard.InterfaceCreateRequest{IsPhysical: standard.PtrBool(true)}
	if useDefaultVpcMetadata {
		if am.VpcNetworkVirtualizationType == FNNVirtualizationType {
			defaultIface.VpcPrefixId = &am.VpcPrefixID
		} else if am.SubnetID != "" {
			defaultIface.SubnetId = &am.SubnetID
		}
	}
	apiReq := standard.InstanceCreateRequest{
		Name:           request.Name,
		TenantId:       am.TenantID,
		VpcId:          vpcID,
		Labels:         request.Labels,
		SshKeyGroupIds: sshKeyGroupIDs,
		Interfaces:     []standard.InterfaceCreateRequest{defaultIface},
	}
	apiReq.MachineId.Set(&request.MachineID)
	if request.Description != nil {
		apiReq.Description.Set(request.Description)
	}
	if request.IpxeScript != "" {
		apiReq.IpxeScript.Set(&request.IpxeScript)
	}
	if request.UserData != nil {
		apiReq.UserData.Set(request.UserData)
	}
	for _, ib := range request.InfinibandInterfaces {
		apiReq.InfinibandInterfaces = append(apiReq.InfinibandInterfaces, toStandardInfiniBandInterface(ib))
	}
	for _, nv := range request.NVLinkInterfaces {
		apiReq.NvLinkInterfaces = append(apiReq.NvLinkInterfaces, toStandardNVLinkInterface(nv))
	}
	for _, d := range request.DpuExtensionServiceDeployments {
		apiReq.DpuExtensionServiceDeployments = append(apiReq.DpuExtensionServiceDeployments, toStandardDpuExtensionDeployment(d))
	}
	return apiReq
}

func toStandardInstanceUpdateRequest(request InstanceUpdateRequest) standard.InstanceUpdateRequest {
	apiReq := standard.InstanceUpdateRequest{Labels: request.Labels}
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
	// Only set slice fields when provided so partial updates do not reset other attributes.
	// When the caller passes nil, no array must be sent so the server skips the field entirely.
	// But if the caller passes an empty array (not nil) we must also pass an empty array.
	if request.InfinibandInterfaces != nil {
		apiReq.InfinibandInterfaces = make([]standard.InfiniBandInterfaceCreateRequest, 0, len(request.InfinibandInterfaces))
		for _, ib := range request.InfinibandInterfaces {
			apiReq.InfinibandInterfaces = append(apiReq.InfinibandInterfaces, toStandardInfiniBandInterface(ib))
		}
	}
	if request.NVLinkInterfaces != nil {
		apiReq.NvLinkInterfaces = make([]standard.NVLinkInterfaceCreateOrUpdateRequest, 0, len(request.NVLinkInterfaces))
		for _, nv := range request.NVLinkInterfaces {
			apiReq.NvLinkInterfaces = append(apiReq.NvLinkInterfaces, toStandardNVLinkInterface(nv))
		}
	}
	if request.DpuExtensionServiceDeployments != nil {
		apiReq.DpuExtensionServiceDeployments = make([]standard.DpuExtensionServiceDeploymentRequest, 0, len(request.DpuExtensionServiceDeployments))
		for _, d := range request.DpuExtensionServiceDeployments {
			apiReq.DpuExtensionServiceDeployments = append(apiReq.DpuExtensionServiceDeployments, toStandardDpuExtensionDeployment(d))
		}
	}
	return apiReq
}

// Create creates a new Instance
func (im InstanceManager) Create(ctx context.Context, request InstanceCreateRequest) (*standard.Instance, *ApiError) {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)

	var sshKeyGroupIDs []string
	if len(request.SSHKeys) > 0 {
		skm := NewSshKeyGroupManager(im.client)
		skg, apiErr := skm.CreateSshKeyGroupForInstance(ctx, request.Name, request.SSHKeys)
		if apiErr != nil {
			return nil, apiErr
		}
		if skg.Id != nil {
			sshKeyGroupIDs = []string{*skg.Id}
		}
	}
	apiReq := toStandardInstanceCreateRequest(request, sshKeyGroupIDs, im.client.apiMetadata)
	apiInst, resp, err := im.client.apiClient.InstanceAPI.CreateInstance(ctx, im.client.apiMetadata.Organization).
		InstanceCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return apiInst, nil
}

// Get returns an Instance by ID
func (im InstanceManager) Get(ctx context.Context, id string) (*standard.Instance, *ApiError) {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)

	apiInst, resp, err := im.client.apiClient.InstanceAPI.GetInstance(ctx, im.client.apiMetadata.Organization, id).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return apiInst, nil
}

// GetInstances returns all Instances
func (im InstanceManager) GetInstances(ctx context.Context, instanceFilter *InstanceFilter, paginationFilter *PaginationFilter) ([]standard.Instance, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)

	gir := im.client.apiClient.InstanceAPI.GetAllInstance(ctx, im.client.apiMetadata.Organization).
		SiteId(im.client.apiMetadata.SiteID)
	if instanceFilter != nil {
		if instanceFilter.Name != nil {
			gir = gir.Name(*instanceFilter.Name)
		}
		if instanceFilter.Query != nil {
			gir = gir.Query(*instanceFilter.Query)
		}
		if instanceFilter.VpcID != nil {
			gir = gir.VpcId(*instanceFilter.VpcID)
		}
		if instanceFilter.IPAddress != nil {
			gir = gir.IpAddress(*instanceFilter.IPAddress)
		}
	}
	// If no explicit VPC filter was provided, fall back to the client's default VPC (if set).
	if (instanceFilter == nil || instanceFilter.VpcID == nil) && im.client.apiMetadata.VpcID != "" {
		gir = gir.VpcId(im.client.apiMetadata.VpcID)
	}
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

	apiInsts, resp, err := gir.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return apiInsts, paginationResponse, nil
}

// Update updates an Instance
func (im InstanceManager) Update(ctx context.Context, id string, request InstanceUpdateRequest) (*standard.Instance, *ApiError) {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)

	apiReq := toStandardInstanceUpdateRequest(request)
	apiInst, resp, err := im.client.apiClient.InstanceAPI.UpdateInstance(ctx, im.client.apiMetadata.Organization, id).
		InstanceUpdateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return apiInst, nil
}

// Delete deletes an Instance.
//
// When the Instance was created with SSHKeys, Create auto-creates an SSH Key
// Group named via GetInstanceSshKeyGroupName. Delete removes that group after
// the Instance is deleted, unless another Instance still references it.
func (im InstanceManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, im.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, im.client.Config.Token)
	logger := LoggerFromContext(ctx)

	var sshKeyGroupIDsToDelete []string
	if instance, getErr := im.Get(ctx, id); getErr == nil {
		sshKeyGroupIDsToDelete = collectAutoCreatedSSHKeyGroupIDs(instance)
		if len(sshKeyGroupIDsToDelete) > 0 {
			sharedIDs, checkErr := im.sshKeyGroupIDsUsedByOtherInstances(ctx, id, sshKeyGroupIDsToDelete)
			if checkErr != nil {
				logger.Warn().Str("instanceId", id).Err(checkErr).
					Msg("failed to check SSH Key Group associations; skipping SSH Key Group cleanup")
				sshKeyGroupIDsToDelete = nil
			} else {
				sshKeyGroupIDsToDelete = filterOutIDs(sshKeyGroupIDsToDelete, sharedIDs)
			}
		}
	} else {
		logger.Warn().Str("instanceId", id).Err(getErr).
			Msg("failed to get Instance; skipping SSH Key Group cleanup")
	}

	_, resp, err := im.client.apiClient.InstanceAPI.DeleteInstance(ctx, im.client.apiMetadata.Organization, id).Execute()
	if apiErr := HandleResponseError(resp, err); apiErr != nil {
		return apiErr
	}

	skm := NewSshKeyGroupManager(im.client)
	for _, skgID := range sshKeyGroupIDsToDelete {
		if delErr := skm.DeleteSshKeyGroup(ctx, skgID); delErr != nil {
			logger.Warn().Str("instanceId", id).Str("sshKeyGroupId", skgID).Err(delErr).
				Msg("failed to delete auto-created SSH Key Group after Instance deletion")
		}
	}
	return nil
}

// collectAutoCreatedSSHKeyGroupIDs returns IDs of SSH Key Groups on the Instance
// that match the naming convention used by CreateSshKeyGroupForInstance.
func collectAutoCreatedSSHKeyGroupIDs(instance *standard.Instance) []string {
	if instance == nil {
		return nil
	}
	expectedName := GetInstanceSshKeyGroupName(instance.GetName())
	var ids []string
	seen := map[string]struct{}{}
	for _, skg := range instance.GetSshKeyGroups() {
		id := skg.GetId()
		if id == "" || skg.GetName() != expectedName {
			continue
		}
		if _, ok := seen[id]; ok {
			continue
		}
		seen[id] = struct{}{}
		ids = append(ids, id)
	}
	return ids
}

// filterOutIDs returns ids with every entry present in exclude removed.
func filterOutIDs(ids []string, exclude map[string]struct{}) []string {
	if len(ids) == 0 || len(exclude) == 0 {
		return ids
	}
	filtered := make([]string, 0, len(ids))
	for _, id := range ids {
		if _, skip := exclude[id]; skip {
			continue
		}
		filtered = append(filtered, id)
	}
	return filtered
}

// sshKeyGroupIDsUsedByOtherInstances returns the subset of candidateIDs that are
// associated with at least one Instance other than excludeInstanceID on the
// client's Site. Listing is site-scoped (not VPC-scoped) so cross-VPC sharing
// is detected.
func (im InstanceManager) sshKeyGroupIDsUsedByOtherInstances(ctx context.Context, excludeInstanceID string, candidateIDs []string) (map[string]struct{}, *ApiError) {
	shared := map[string]struct{}{}
	if len(candidateIDs) == 0 {
		return shared, nil
	}
	candidates := make(map[string]struct{}, len(candidateIDs))
	for _, id := range candidateIDs {
		candidates[id] = struct{}{}
	}

	pageNumber := int32(1)
	pageSize := int32(100)
	for {
		instances, resp, err := im.client.apiClient.InstanceAPI.GetAllInstance(ctx, im.client.apiMetadata.Organization).
			SiteId(im.client.apiMetadata.SiteID).
			PageNumber(pageNumber).
			PageSize(pageSize).
			Execute()
		if apiErr := HandleResponseError(resp, err); apiErr != nil {
			return nil, apiErr
		}

		for _, inst := range instances {
			if inst.GetId() == excludeInstanceID {
				continue
			}
			for _, skgID := range inst.GetSshKeyGroupIds() {
				if _, ok := candidates[skgID]; ok {
					shared[skgID] = struct{}{}
				}
			}
			for _, skg := range inst.GetSshKeyGroups() {
				if skgID := skg.GetId(); skgID != "" {
					if _, ok := candidates[skgID]; ok {
						shared[skgID] = struct{}{}
					}
				}
			}
		}

		// All candidates are shared; no need to keep paging.
		if len(shared) == len(candidates) {
			return shared, nil
		}

		pagination, perr := standard.GetPaginationResponse(ctx, resp)
		if perr != nil {
			return nil, &ApiError{
				Code:    http.StatusInternalServerError,
				Message: "failed to extract pagination: " + perr.Error(),
				Data:    map[string]interface{}{"parseError": perr.Error()},
			}
		}
		if len(instances) == 0 || int(pageNumber)*pagination.PageSize >= pagination.Total {
			break
		}
		pageNumber++
	}
	return shared, nil
}
