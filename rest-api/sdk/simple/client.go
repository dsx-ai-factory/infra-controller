// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"errors"
	"net/http"
	"os"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// ClientInterface defines the methods for the simple SDK Client
type ClientInterface interface {
	// Authentication management interfaces
	Authenticate(ctx context.Context) error
	UpdateToken(ctx context.Context, token string) error

	// Instance management interfaces
	CreateInstance(ctx context.Context, request InstanceCreateRequest) (*standard.Instance, *ApiError)
	GetInstances(ctx context.Context, instanceFilter *InstanceFilter, paginationFilter *PaginationFilter) ([]standard.Instance, *standard.PaginationResponse, *ApiError)
	GetInstance(ctx context.Context, id string) (*standard.Instance, *ApiError)
	UpdateInstance(ctx context.Context, id string, request InstanceUpdateRequest) (*standard.Instance, *ApiError)
	DeleteInstance(ctx context.Context, id string) *ApiError

	// IP Block Management interfaces
	GetIpBlocks(ctx context.Context, paginationFilter *PaginationFilter) ([]IpBlock, *standard.PaginationResponse, *ApiError)
	GetIpBlock(ctx context.Context, id string) (*IpBlock, *ApiError)

	// InfiniBand Partition management interfaces
	CreateInfinibandPartition(ctx context.Context, request InfinibandPartitionCreateRequest) (*InfinibandPartition, *ApiError)
	GetInfinibandPartitions(ctx context.Context, paginationFilter *PaginationFilter) ([]InfinibandPartition, *standard.PaginationResponse, *ApiError)
	GetInfinibandPartition(ctx context.Context, id string) (*InfinibandPartition, *ApiError)
	UpdateInfinibandPartition(ctx context.Context, id string, request InfinibandPartitionUpdateRequest) (*InfinibandPartition, *ApiError)
	DeleteInfinibandPartition(ctx context.Context, id string) *ApiError

	// Machine management interfaces
	GetMachines(ctx context.Context, paginationFilter *PaginationFilter) ([]Machine, *standard.PaginationResponse, *ApiError)
	GetMachine(ctx context.Context, id string) (*Machine, *ApiError)
	GetDpuMachines(ctx context.Context, machineID string) ([]standard.DpuMachine, *ApiError)

	// Expected Machine management interfaces
	CreateExpectedMachine(ctx context.Context, request ExpectedMachineCreateRequest) (*ExpectedMachine, *ApiError)
	GetExpectedMachines(ctx context.Context, paginationFilter *PaginationFilter) ([]ExpectedMachine, *standard.PaginationResponse, *ApiError)
	GetExpectedMachine(ctx context.Context, id string) (*ExpectedMachine, *ApiError)
	UpdateExpectedMachine(ctx context.Context, id string, request ExpectedMachineUpdateRequest) (*ExpectedMachine, *ApiError)
	DeleteExpectedMachine(ctx context.Context, id string) *ApiError
	BatchCreateExpectedMachines(ctx context.Context, requests []ExpectedMachineCreateRequest) ([]ExpectedMachine, *ApiError)
	BatchUpdateExpectedMachines(ctx context.Context, updates []ExpectedMachineUpdateRequest) ([]ExpectedMachine, *ApiError)

	// Operating System management interfaces
	CreateOperatingSystem(ctx context.Context, request OperatingSystemCreateRequest) (*OperatingSystem, *ApiError)
	GetOperatingSystems(ctx context.Context, paginationFilter *PaginationFilter) ([]OperatingSystem, *standard.PaginationResponse, *ApiError)
	GetOperatingSystem(ctx context.Context, id string) (*OperatingSystem, *ApiError)
	UpdateOperatingSystem(ctx context.Context, id string, request OperatingSystemUpdateRequest) (*OperatingSystem, *ApiError)
	DeleteOperatingSystem(ctx context.Context, id string) *ApiError

	// NVLink Logical Partition management interfaces
	CreateNVLinkLogicalPartition(ctx context.Context, request NVLinkLogicalPartitionCreateRequest) (*NVLinkLogicalPartition, *ApiError)
	GetNVLinkLogicalPartitions(ctx context.Context, paginationFilter *PaginationFilter) ([]NVLinkLogicalPartition, *standard.PaginationResponse, *ApiError)
	GetNVLinkLogicalPartition(ctx context.Context, id string) (*NVLinkLogicalPartition, *ApiError)
	UpdateNVLinkLogicalPartition(ctx context.Context, id string, request NVLinkLogicalPartitionUpdateRequest) (*NVLinkLogicalPartition, *ApiError)
	DeleteNVLinkLogicalPartition(ctx context.Context, id string) *ApiError

	// DPU Extension Service management interfaces
	CreateDpuExtensionService(ctx context.Context, request DpuExtensionServiceCreateRequest) (*DpuExtensionService, *ApiError)
	GetDpuExtensionServices(ctx context.Context, paginationFilter *PaginationFilter) ([]DpuExtensionService, *standard.PaginationResponse, *ApiError)
	GetDpuExtensionService(ctx context.Context, id string) (*DpuExtensionService, *ApiError)
	UpdateDpuExtensionService(ctx context.Context, id string, request DpuExtensionServiceUpdateRequest) (*DpuExtensionService, *ApiError)
	DeleteDpuExtensionService(ctx context.Context, id string) *ApiError
	GetDpuExtensionServiceVersion(ctx context.Context, id string, version string) (*standard.DpuExtensionServiceVersionInfo, *ApiError)
	DeleteDpuExtensionServiceVersion(ctx context.Context, id string, version string) *ApiError

	// VPC management interfaces
	CreateVpc(ctx context.Context, request VpcCreateRequest) (*Vpc, *ApiError)
	GetVpcs(ctx context.Context, vpcFilter *VpcFilter, paginationFilter *PaginationFilter) ([]Vpc, *standard.PaginationResponse, *ApiError)
	GetVpc(ctx context.Context, id string) (*Vpc, *ApiError)
	UpdateVpc(ctx context.Context, id string, request VpcUpdateRequest) (*Vpc, *ApiError)
	DeleteVpc(ctx context.Context, id string) *ApiError
}

// Ensure *Client implements ClientInterface at compile time
var _ ClientInterface = (*Client)(nil)

// ClientConfig is a struct that contains the configuration for the client
type ClientConfig struct {
	// BaseURL is the base URL of NICo REST API. For in-cluster requests, use "https://nico-rest-api.nico-rest.svc.cluster.local"
	BaseURL string
	// Org is the organization to use for the client. Select desired service org from const.go.
	Org string
	// APIName overrides the API path segment after /org/{org}/. Leave empty to use the default nico path.
	APIName string
	// Token should contain a valid JWT
	Token string
	// Logger is the logger instance to use for SDK logging. If nil, a no-op logger will be used by default.
	Logger Logger
}

// Client is a struct that contains the client for the NICo API
type Client struct {
	// The configuration for the client supplied by the SDK user
	Config ClientConfig
	// The client for the API
	apiClient *standard.APIClient
	// Internal metadata used for communication with the API
	apiMetadata ApiMetadata
	// Logger is the logger instance used by the client
	Logger Logger
}

// Authenticate initiate session with nico-rest-api/keycloak and retrieve JWT.
// It also makes an API call to retrieve service-specific information to cache.
func (c *Client) Authenticate(ctx context.Context) error {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Initializing API client for org: %s", c.Config.Org)

	apiConfig := standard.NewConfiguration()
	apiConfig.Servers = standard.ServerConfigurations{
		{URL: c.Config.BaseURL, Description: "Local"},
	}
	apiConfig.SetAPIName(c.Config.APIName)

	c.apiClient = standard.NewAPIClient(apiConfig)

	logger.Info().Msgf("Initializing API metadata for org: %s", c.Config.Org)
	apiErr := c.apiMetadata.Initialize(ctx, c.Config.Org, c.apiClient)
	if apiErr != nil {
		return apiErr
	}

	if _, ok := c.IsMinimumAPIVersion("v0.2.86"); !ok {
		return &ApiError{Code: http.StatusUpgradeRequired, Message: "minimum supported API version is v0.2.86; please upgrade your deployment or downgrade your SDK version"}
	}
	return nil
}

// UpdateToken updates the JWT token and re-authenticates
func (c *Client) UpdateToken(ctx context.Context, token string) error {
	ctx = WithLogger(ctx, c.Logger)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating JWT token for org: %s", c.Config.Org)

	c.Config.Token = token
	return c.Authenticate(ctx)
}

// SetSiteID sets the Site ID for the client. Can be used to override the default Site ID for testing.
func (c *Client) SetSiteID(siteID string) {
	c.apiMetadata.SiteID = siteID
}

// GetSiteID returns the current Site ID from the client
func (c *Client) GetSiteID() string {
	return c.apiMetadata.SiteID
}

// SetVpcID sets the VPC ID for the client
func (c *Client) SetVpcID(vpcID string) {
	c.apiMetadata.VpcID = vpcID
}

// GetVpcID returns the current VPC ID from the client
func (c *Client) GetVpcID() string {
	return c.apiMetadata.VpcID
}

// SetVpcPrefixID sets the VPC Prefix ID for the client
func (c *Client) SetVpcPrefixID(vpcPrefixID string) {
	c.apiMetadata.VpcPrefixID = vpcPrefixID
}

// GetVpcPrefixID returns the current VPC Prefix ID from the client
func (c *Client) GetVpcPrefixID() string {
	return c.apiMetadata.VpcPrefixID
}

// SetSubnetID sets the Subnet ID for the client
func (c *Client) SetSubnetID(subnetID string) {
	c.apiMetadata.SubnetID = subnetID
}

// GetSubnetID returns the current Subnet ID from the client
func (c *Client) GetSubnetID() string {
	return c.apiMetadata.SubnetID
}

// IsMinimumAPIVersion returns the API version and whether it meets the required minimum
func (c *Client) IsMinimumAPIVersion(requiredVersion string) (string, bool) {
	return c.apiMetadata.IsMinimumAPIVersion(requiredVersion)
}

// Instance
func (c *Client) CreateInstance(ctx context.Context, request InstanceCreateRequest) (*standard.Instance, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating Instance for org: %s", c.Config.Org)

	return NewInstanceManager(c).Create(ctx, request)
}
func (c *Client) DeleteInstance(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting Instance for org: %s", c.Config.Org)

	return NewInstanceManager(c).Delete(ctx, id)
}
func (c *Client) GetInstances(ctx context.Context, instanceFilter *InstanceFilter, paginationFilter *PaginationFilter) ([]standard.Instance, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all Instances for org: %s", c.Config.Org)

	return NewInstanceManager(c).GetInstances(ctx, instanceFilter, paginationFilter)
}
func (c *Client) GetInstance(ctx context.Context, id string) (*standard.Instance, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting Instance for org: %s", c.Config.Org)

	return NewInstanceManager(c).Get(ctx, id)
}
func (c *Client) UpdateInstance(ctx context.Context, id string, request InstanceUpdateRequest) (*standard.Instance, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating Instance for org: %s", c.Config.Org)

	return NewInstanceManager(c).Update(ctx, id, request)
}

// IpBlock
func (c *Client) GetIpBlocks(ctx context.Context, paginationFilter *PaginationFilter) ([]IpBlock, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all IP Blocks for org: %s", c.Config.Org)

	return NewIpBlockManager(c).GetIpBlocks(ctx, paginationFilter)
}
func (c *Client) GetIpBlock(ctx context.Context, id string) (*IpBlock, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting IP Block for org: %s", c.Config.Org)

	return NewIpBlockManager(c).GetIpBlock(ctx, id)
}

// InfinibandPartition
func (c *Client) CreateInfinibandPartition(ctx context.Context, request InfinibandPartitionCreateRequest) (*InfinibandPartition, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating InfiniBand Partition for org: %s", c.Config.Org)

	return NewInfinibandPartitionManager(c).Create(ctx, request)
}
func (c *Client) UpdateInfinibandPartition(ctx context.Context, id string, request InfinibandPartitionUpdateRequest) (*InfinibandPartition, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating InfiniBand Partition for org: %s", c.Config.Org)

	return NewInfinibandPartitionManager(c).Update(ctx, id, request)
}
func (c *Client) GetInfinibandPartitions(ctx context.Context, paginationFilter *PaginationFilter) ([]InfinibandPartition, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all InfiniBand Partitions for org: %s", c.Config.Org)

	return NewInfinibandPartitionManager(c).GetInfinibandPartitions(ctx, paginationFilter)
}
func (c *Client) GetInfinibandPartition(ctx context.Context, id string) (*InfinibandPartition, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting InfiniBand Partition for org: %s", c.Config.Org)

	return NewInfinibandPartitionManager(c).Get(ctx, id)
}
func (c *Client) DeleteInfinibandPartition(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting InfiniBand Partition for org: %s", c.Config.Org)

	return NewInfinibandPartitionManager(c).Delete(ctx, id)
}

// Machine
func (c *Client) GetMachines(ctx context.Context, paginationFilter *PaginationFilter) ([]Machine, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all Machines for org: %s", c.Config.Org)

	return NewMachineManager(c).GetMachines(ctx, paginationFilter)
}
func (c *Client) GetMachine(ctx context.Context, id string) (*Machine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting Machine for org: %s", c.Config.Org)

	return NewMachineManager(c).GetMachine(ctx, id)
}

func (c *Client) GetDpuMachines(ctx context.Context, machineID string) ([]standard.DpuMachine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting DPU Machines for host Machine %s in org: %s", machineID, c.Config.Org)

	return NewMachineManager(c).GetDpuMachines(ctx, machineID)
}

// ExpectedMachine
func (c *Client) CreateExpectedMachine(ctx context.Context, request ExpectedMachineCreateRequest) (*ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating Expected Machine for org: %s", c.Config.Org)

	return NewExpectedMachineManager(c).Create(ctx, request)
}
func (c *Client) GetExpectedMachines(ctx context.Context, paginationFilter *PaginationFilter) ([]ExpectedMachine, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all Expected Machines for org: %s", c.Config.Org)

	return NewExpectedMachineManager(c).GetExpectedMachines(ctx, paginationFilter)
}
func (c *Client) GetExpectedMachine(ctx context.Context, id string) (*ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting Expected Machine for org: %s", c.Config.Org)

	return NewExpectedMachineManager(c).Get(ctx, id)
}
func (c *Client) UpdateExpectedMachine(ctx context.Context, id string, request ExpectedMachineUpdateRequest) (*ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating Expected Machine for org: %s", c.Config.Org)

	return NewExpectedMachineManager(c).Update(ctx, id, request)
}
func (c *Client) DeleteExpectedMachine(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting Expected Machine for org: %s", c.Config.Org)

	return NewExpectedMachineManager(c).Delete(ctx, id)
}
func (c *Client) BatchCreateExpectedMachines(ctx context.Context, requests []ExpectedMachineCreateRequest) ([]ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Batch creating %d Expected Machines for org: %s", len(requests), c.Config.Org)

	return NewExpectedMachineManager(c).BatchCreate(ctx, requests)
}
func (c *Client) BatchUpdateExpectedMachines(ctx context.Context, updates []ExpectedMachineUpdateRequest) ([]ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Batch updating %d Expected Machines for org: %s", len(updates), c.Config.Org)

	return NewExpectedMachineManager(c).BatchUpdate(ctx, updates)
}

// OperatingSystem
func (c *Client) CreateOperatingSystem(ctx context.Context, request OperatingSystemCreateRequest) (*OperatingSystem, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating Operating System for org: %s", c.Config.Org)

	return NewOperatingSystemManager(c).Create(ctx, request)
}
func (c *Client) GetOperatingSystems(ctx context.Context, paginationFilter *PaginationFilter) ([]OperatingSystem, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all Operating Systems for org: %s", c.Config.Org)

	return NewOperatingSystemManager(c).GetOperatingSystems(ctx, paginationFilter)
}
func (c *Client) GetOperatingSystem(ctx context.Context, id string) (*OperatingSystem, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting Operating System for org: %s", c.Config.Org)

	return NewOperatingSystemManager(c).Get(ctx, id)
}
func (c *Client) UpdateOperatingSystem(ctx context.Context, id string, request OperatingSystemUpdateRequest) (*OperatingSystem, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating Operating System for org: %s", c.Config.Org)

	return NewOperatingSystemManager(c).Update(ctx, id, request)
}
func (c *Client) DeleteOperatingSystem(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting Operating System for org: %s", c.Config.Org)

	return NewOperatingSystemManager(c).Delete(ctx, id)
}

// SshKeyGroup
func (c *Client) CreateSshKey(ctx context.Context, sshPublicKey string) (*standard.SshKey, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating SSH Key for org: %s", c.Config.Org)

	return NewSshKeyGroupManager(c).CreateSshKey(ctx, sshPublicKey)
}
func (c *Client) CreateSshKeyGroupForInstance(ctx context.Context, instanceName string, sshPublicKeys []string) (*standard.SshKeyGroup, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating SSH Key Group for Instance for org: %s", c.Config.Org)

	return NewSshKeyGroupManager(c).CreateSshKeyGroupForInstance(ctx, instanceName, sshPublicKeys)
}
func (c *Client) GetSshKeyGroup(ctx context.Context, sshKeyGroupID string) (*standard.SshKeyGroup, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting SSH Key Group for org: %s", c.Config.Org)

	return NewSshKeyGroupManager(c).GetSshKeyGroup(ctx, sshKeyGroupID)
}
func (c *Client) DeleteSshKeyGroup(ctx context.Context, sshKeyGroupID string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting SSH Key Group for org: %s", c.Config.Org)

	return NewSshKeyGroupManager(c).DeleteSshKeyGroup(ctx, sshKeyGroupID)
}

// NVLinkLogicalPartition
func (c *Client) CreateNVLinkLogicalPartition(ctx context.Context, request NVLinkLogicalPartitionCreateRequest) (*NVLinkLogicalPartition, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating NVLink Logical Partition for org: %s", c.Config.Org)

	return NewNVLinkLogicalPartitionManager(c).Create(ctx, request)
}
func (c *Client) GetNVLinkLogicalPartitions(ctx context.Context, paginationFilter *PaginationFilter) ([]NVLinkLogicalPartition, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all NVLink Logical Partitions for org: %s", c.Config.Org)

	return NewNVLinkLogicalPartitionManager(c).GetNVLinkLogicalPartitions(ctx, paginationFilter)
}
func (c *Client) GetNVLinkLogicalPartition(ctx context.Context, id string) (*NVLinkLogicalPartition, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting NVLink Logical Partition for org: %s", c.Config.Org)

	return NewNVLinkLogicalPartitionManager(c).Get(ctx, id)
}
func (c *Client) UpdateNVLinkLogicalPartition(ctx context.Context, id string, request NVLinkLogicalPartitionUpdateRequest) (*NVLinkLogicalPartition, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating NVLink Logical Partition for org: %s", c.Config.Org)

	return NewNVLinkLogicalPartitionManager(c).Update(ctx, id, request)
}
func (c *Client) DeleteNVLinkLogicalPartition(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting NVLink Logical Partition for org: %s", c.Config.Org)

	return NewNVLinkLogicalPartitionManager(c).Delete(ctx, id)
}

// DpuExtensionService
func (c *Client) CreateDpuExtensionService(ctx context.Context, request DpuExtensionServiceCreateRequest) (*DpuExtensionService, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating DPU Extension Service for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).Create(ctx, request)
}
func (c *Client) GetDpuExtensionServices(ctx context.Context, paginationFilter *PaginationFilter) ([]DpuExtensionService, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all DPU Extension Services for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).GetDpuExtensionServices(ctx, paginationFilter)
}
func (c *Client) GetDpuExtensionService(ctx context.Context, id string) (*DpuExtensionService, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting DPU Extension Service for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).Get(ctx, id)
}
func (c *Client) UpdateDpuExtensionService(ctx context.Context, id string, request DpuExtensionServiceUpdateRequest) (*DpuExtensionService, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating DPU Extension Service for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).Update(ctx, id, request)
}
func (c *Client) DeleteDpuExtensionService(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting DPU Extension Service for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).Delete(ctx, id)
}
func (c *Client) GetDpuExtensionServiceVersion(ctx context.Context, id string, version string) (*standard.DpuExtensionServiceVersionInfo, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting DPU Extension Service Version for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).GetDpuExtensionServiceVersion(ctx, id, version)
}
func (c *Client) DeleteDpuExtensionServiceVersion(ctx context.Context, id string, version string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting DPU Extension Service Version for org: %s", c.Config.Org)

	return NewDpuExtensionServiceManager(c).DeleteDpuExtensionServiceVersion(ctx, id, version)
}

// VPC
func (c *Client) CreateVpc(ctx context.Context, request VpcCreateRequest) (*Vpc, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Creating VPC for org: %s", c.Config.Org)

	return NewVpcManager(c).CreateVpc(ctx, request)
}
func (c *Client) GetVpcs(ctx context.Context, vpcFilter *VpcFilter, paginationFilter *PaginationFilter) ([]Vpc, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting all VPCs for org: %s", c.Config.Org)

	return NewVpcManager(c).GetVpcs(ctx, vpcFilter, paginationFilter)
}
func (c *Client) GetVpc(ctx context.Context, id string) (*Vpc, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Getting VPC for org: %s", c.Config.Org)

	return NewVpcManager(c).GetVpc(ctx, id)
}
func (c *Client) UpdateVpc(ctx context.Context, id string, request VpcUpdateRequest) (*Vpc, *ApiError) {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Updating VPC for org: %s", c.Config.Org)

	return NewVpcManager(c).UpdateVpc(ctx, id, request)
}
func (c *Client) DeleteVpc(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, c.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, c.Config.Token)

	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Deleting VPC for org: %s", c.Config.Org)

	return NewVpcManager(c).DeleteVpc(ctx, id)
}

// NewClient creates a new simple SDK client
func NewClient(config ClientConfig) (*Client, error) {
	if config.BaseURL == "" {
		return nil, errors.New("base URL is required")
	}
	if config.Org == "" {
		return nil, errors.New("org is required")
	}
	if config.Token == "" {
		return nil, errors.New("token is required")
	}
	if config.Logger == nil {
		config.Logger = NewNoOpLogger()
	}

	return &Client{
		Config: config,
		Logger: config.Logger,
	}, nil
}

// NewClientFromEnv creates a new client from environment variables.
// NICO_* variables are preferred; the legacy CARBIDE_* variables are
// honoured as a fallback so callers can migrate gradually.
func NewClientFromEnv() (*Client, error) {
	config := ClientConfig{
		BaseURL: envWithFallback("NICO_BASE_URL", "CARBIDE_BASE_URL"),
		Org:     envWithFallback("NICO_ORG", "CARBIDE_ORG"),
		APIName: envWithFallback("NICO_API_NAME", "CARBIDE_API_NAME"),
		Token:   envWithFallback("NICO_TOKEN", "CARBIDE_TOKEN"),
	}
	if config.Token == "" {
		if apiKey := envWithFallback("NICO_API_KEY", "CARBIDE_API_KEY"); apiKey != "" {
			config.Token = apiKey
		} else {
			return nil, errors.New("NICO_TOKEN env var (or alternatively NICO_API_KEY) must be set")
		}
	}
	return NewClient(config)
}

// envWithFallback returns the value of primary if set, else fallback.
func envWithFallback(primary, fallback string) string {
	if v := os.Getenv(primary); v != "" {
		return v
	}
	return os.Getenv(fallback)
}

// NewClientFromEnvWithLogger creates a new client from environment variables with the specified logger
func NewClientFromEnvWithLogger(logger Logger) (*Client, error) {
	client, err := NewClientFromEnv()
	if err != nil {
		return nil, err
	}
	if logger == nil {
		logger = NewNoOpLogger()
	}
	client.Config.Logger = logger
	client.Logger = logger
	return client, nil
}
