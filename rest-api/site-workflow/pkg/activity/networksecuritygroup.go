// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"time"

	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"

	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

// ManageNetworkSecurityGroup is an activity wrapper for NetworkSecurityGroup management tasks that allows injecting DB access
type ManageNetworkSecurityGroup struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// Function to Create NICo NetworkSecurityGroup with the Site Controller
func (mm *ManageNetworkSecurityGroup) CreateNetworkSecurityGroupOnSite(ctx context.Context, request *corev1.CreateNetworkSecurityGroupRequest) error {
	logger := log.With().Str("Activity", "CreateNetworkSecurityGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty create NetworkSecurityGroup request")
	case request.Id == nil || *request.Id == "":
		err = errors.New("received create NetworkSecurityGroup request without ID")
	case request.TenantOrganizationId == "":
		err = errors.New("received create NetworkSecurityGroup request with empty Tenant ID")

	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	rpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = rpcServiceClient.CreateNetworkSecurityGroup(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create NetworkSecurityGroup using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function Update NICo NetworkSecurityGroup with the Site Controller
func (mm *ManageNetworkSecurityGroup) UpdateNetworkSecurityGroupOnSite(ctx context.Context, request *corev1.UpdateNetworkSecurityGroupRequest) error {
	logger := log.With().Str("Activity", "UpdateNetworkSecurityGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty NetworkSecurityGroup config update request")
	case request.Id == "":
		err = errors.New("received NetworkSecurityGroup config update request without NetworkSecurityGroup ID")
	case request.TenantOrganizationId == "":
		err = errors.New("received NetworkSecurityGroup config update request without Tenant ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	rpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = rpcServiceClient.UpdateNetworkSecurityGroup(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update config for NetworkSecurityGroup using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Delete NICo NetworkSecurityGroup with the Site Controller
func (mm *ManageNetworkSecurityGroup) DeleteNetworkSecurityGroupOnSite(ctx context.Context, request *corev1.DeleteNetworkSecurityGroupRequest) error {
	logger := log.With().Str("Activity", "DeleteNetworkSecurityGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty delete NetworkSecurityGroup request")
	case request.Id == "":
		err = errors.New("received delete NetworkSecurityGroup request without NetworkSecurityGroup ID")
	case request.TenantOrganizationId == "":
		err = errors.New("received delete NetworkSecurityGroup request without empty Tenant ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	rpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = rpcServiceClient.DeleteNetworkSecurityGroup(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete NetworkSecurityGroup using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// NewManageNetworkSecurityGroup returns a new ManageNetworkSecurityGroup activity
func NewManageNetworkSecurityGroup(coreGrpcClient *cClient.CoreGrpcAtomicClient) ManageNetworkSecurityGroup {
	return ManageNetworkSecurityGroup{
		coreGrpcAtomicClient: coreGrpcClient,
	}
}

// ManageNetworkSecurityGroupInventory is an activity wrapper for NetworkSecurityGroup inventory collection and publishing
type ManageNetworkSecurityGroupInventory struct {
	config ManageInventoryConfig
}

// DiscoverNetworkSecurityGroupInventory is an activity to collect NetworkSecurityGroup inventory and publish to Temporal queue
func (mmi *ManageNetworkSecurityGroupInventory) DiscoverNetworkSecurityGroupInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverNetworkSecurityGroupInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.UUID, *corev1.NetworkSecurityGroup, *corev1.NetworkSecurityGroupInventory]{
		itemType:               "NetworkSecurityGroup",
		config:                 mmi.config,
		internalFindIDs:        networkSecurityGroupFindIDs,
		internalFindByIDs:      networkSecurityGroupFindByIDs,
		internalPagedInventory: networkSecurityGroupPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageNetworkSecurityGroupInventory returns a ManageInventory implementation for NetworkSecurityGroup activity
func NewManageNetworkSecurityGroupInventory(config ManageInventoryConfig) ManageNetworkSecurityGroupInventory {
	return ManageNetworkSecurityGroupInventory{
		config: config,
	}
}

func networkSecurityGroupFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.UUID, error) {
	// Call Core gRPC API endpoint
	grpcServiceClient := grpcClient.GrpcServiceClient()
	networkSecurityGroupIdList, err := grpcServiceClient.FindNetworkSecurityGroupIds(ctx, &corev1.FindNetworkSecurityGroupIdsRequest{})
	if err != nil {
		return nil, err
	}
	return util.StringsToProtobufUUIDList(networkSecurityGroupIdList.GetNetworkSecurityGroupIds()), nil
}

func networkSecurityGroupFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.UUID) ([]*corev1.NetworkSecurityGroup, error) {
	nsgIDs := make([]string, len(ids))

	for i, id := range ids {
		nsgIDs[i] = id.GetValue()
	}

	// Call Core gRPC API endpoint
	grpcServiceClient := grpcClient.GrpcServiceClient()
	networkSecurityGroupList, err := grpcServiceClient.FindNetworkSecurityGroupsByIds(ctx, &corev1.FindNetworkSecurityGroupsByIdsRequest{
		NetworkSecurityGroupIds: nsgIDs,
	})
	if err != nil {
		return nil, err
	}
	return networkSecurityGroupList.GetNetworkSecurityGroups(), nil
}

func networkSecurityGroupPagedInventory(allItemIDs []*corev1.UUID, pagedItems []*corev1.NetworkSecurityGroup, input *pagedInventoryInput) *corev1.NetworkSecurityGroupInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of Machines
	networkSecurityGroupInventory := &corev1.NetworkSecurityGroupInventory{
		NetworkSecurityGroups: pagedItems,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if networkSecurityGroupInventory.InventoryPage != nil {
		networkSecurityGroupInventory.InventoryPage.ItemIds = itemIDs
	}
	return networkSecurityGroupInventory
}
