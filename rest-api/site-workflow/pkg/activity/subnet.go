// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"net"
	"time"

	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// ManageSubnet is an activity wrapper for Subnet management tasks that allows injecting DB access
type ManageSubnet struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// Function to Create Subnets with the Site Controller
func (mm *ManageSubnet) CreateSubnetOnSite(ctx context.Context, request *corev1.NetworkSegmentCreationRequest) error {
	logger := log.With().Str("Activity", "CreateSubnetOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty create Subnet request")
	case request.Name == "":
		err = errors.New("received create Subnet request without name")
	case request.VpcId == nil:
		err = errors.New("received create Subnet request without VPC ID")
	case len(request.Prefixes) == 0:
		err = errors.New("received create Subnet request with empty prefix list")
	case len(request.Prefixes) > 0:
		for _, prefix := range request.Prefixes {
			if prefix == nil {
				err = errors.New("received create Subnet request with a nil prefix in the prefix list")
				break
			}
			if _, _, err = net.ParseCIDR(prefix.Prefix); err != nil {
				err = errors.New("received create Subnet request with an invalid prefix in the prefix list")
				break
			}
		}
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.CreateNetworkSegment(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Subnet using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Delete Subnets with the Site Controller
func (mm *ManageSubnet) DeleteSubnetOnSite(ctx context.Context, request *corev1.NetworkSegmentDeletionRequest) error {
	logger := log.With().Str("Activity", "DeleteSubnetOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty delete Subnet request")
	case request.Id == nil:
		err = errors.New("received delete Subnet request without subnet ID")
	case request.Id.Value == "":
		err = errors.New("received delete Subnet request with empty subnet ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteNetworkSegment(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete Subnet using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// NewManageSubnet returns a new ManageSubnet client
func NewManageSubnet(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageSubnet {
	return ManageSubnet{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// ManageSubnetInventory is an activity wrapper for Subnet inventory collection and publishing
type ManageSubnetInventory struct {
	config ManageInventoryConfig
}

// DiscoverSubnetInventory is an activity to collect Subnet inventory and publish to Temporal queue
func (mmi *ManageSubnetInventory) DiscoverSubnetInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverSubnetInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.NetworkSegmentId, *corev1.NetworkSegment, *corev1.SubnetInventory]{
		itemType:               "Subnet",
		config:                 mmi.config,
		internalFindIDs:        subnetFindIDs,
		internalFindByIDs:      subnetFindByIDs,
		internalPagedInventory: subnetPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageSubnetInventory returns a ManageInventory implementation for Subnet activity
func NewManageSubnetInventory(config ManageInventoryConfig) ManageSubnetInventory {
	return ManageSubnetInventory{
		config: config,
	}
}

func subnetFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.NetworkSegmentId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.FindNetworkSegmentIds(ctx, &corev1.NetworkSegmentSearchFilter{})
	if err != nil {
		return nil, err
	}
	return idList.GetNetworkSegmentsIds(), nil
}

func subnetFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.NetworkSegmentId) ([]*corev1.NetworkSegment, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.FindNetworkSegmentsByIds(ctx, &corev1.NetworkSegmentsByIdsRequest{
		NetworkSegmentsIds: ids,
	})
	if err != nil {
		return nil, err
	}
	return list.GetNetworkSegments(), nil
}

func subnetPagedInventory(allItemIDs []*corev1.NetworkSegmentId, pagedItems []*corev1.NetworkSegment, input *pagedInventoryInput) *corev1.SubnetInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page
	inventory := &corev1.SubnetInventory{
		Segments: pagedItems,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if inventory.InventoryPage != nil {
		inventory.InventoryPage.ItemIds = itemIDs
	}
	return inventory
}
