// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	cclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ManageInfiniBandPartitionInventory is an activity wrapper for InfiniBand Partition inventory collection and publishing
type ManageInfiniBandPartitionInventory struct {
	config ManageInventoryConfig
}

// DiscoverInfiniBandPartitionInventory is an activity to collect InfiniBand Partition inventory and publish to Temporal queue
func (mmi *ManageInfiniBandPartitionInventory) DiscoverInfiniBandPartitionInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverIBPartitionInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.IBPartitionId, *corev1.IBPartition, *corev1.InfiniBandPartitionInventory]{
		itemType:               "InfiniBandPartition",
		config:                 mmi.config,
		internalFindIDs:        ibpFindIDs,
		internalFindByIDs:      ibpFindByIDs,
		internalPagedInventory: ibpPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageInfiniBandPartitionInventory returns a ManageInventory implementation for InfiniBand Partition activity
func NewManageInfiniBandPartitionInventory(config ManageInventoryConfig) ManageInfiniBandPartitionInventory {
	return ManageInfiniBandPartitionInventory{
		config: config,
	}
}

func ibpFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.IBPartitionId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.FindIBPartitionIds(ctx, &corev1.IBPartitionSearchFilter{})
	if err != nil {
		return nil, err
	}
	return idList.GetIbPartitionIds(), nil
}

func ibpFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.IBPartitionId) ([]*corev1.IBPartition, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.FindIBPartitionsByIds(ctx, &corev1.IBPartitionsByIdsRequest{
		IbPartitionIds: ids,
	})
	if err != nil {
		return nil, err
	}
	return list.GetIbPartitions(), nil
}

func ibpPagedInventory(allItemIDs []*corev1.IBPartitionId, pagedItems []*corev1.IBPartition, input *pagedInventoryInput) *corev1.InfiniBandPartitionInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of VPCs
	inventory := &corev1.InfiniBandPartitionInventory{
		IbPartitions: pagedItems,
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

// ManageInfiniBandPartition is an activity wrapper for InfiniBand Partition management
type ManageInfiniBandPartition struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// NewManageInfiniBandPartition returns a new ManageInfiniBandPartition client
func NewManageInfiniBandPartition(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageInfiniBandPartition {
	return ManageInfiniBandPartition{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// Function to create InfiniBand Partition with NICo
func (mibp *ManageInfiniBandPartition) CreateInfiniBandPartitionOnSite(ctx context.Context, request *corev1.IBPartitionCreationRequest) error {
	logger := log.With().Str("Activity", "CreateInfiniBandPartitionOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create InfiniBand Partition request")
	} else if request.Id == nil || request.GetId().GetValue() == "" {
		err = errors.New("received create InfiniBand Partition request without ID")
	} else if request.GetConfig() == nil {
		err = errors.New("received create InfiniBand Partition request without Config")
	} else if request.GetMetadata().GetName() == "" && request.GetConfig().GetName() == "" {
		// Backward compatibility: both Metadata.Name and Config.Name are accepted
		err = errors.New("received create InfiniBand Partition request without Name")
	} else if request.GetConfig().GetTenantOrganizationId() == "" {
		err = errors.New("received create InfiniBand Partition request without TenantOrganizationId")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mibp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call Core gRPC endpoint
	start := time.Now()
	_, err = grpcServiceClient.CreateIBPartition(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create InfiniBand Partition using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// UpdateInfiniBandPartitionOnSite applies an IB partition update on the site NICo controller
func (mibp *ManageInfiniBandPartition) UpdateInfiniBandPartitionOnSite(ctx context.Context, request *corev1.IBPartitionUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateInfiniBandPartitionOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	if request == nil {
		err = errors.New("received empty update InfiniBand Partition request")
	} else if request.Id == nil || request.GetId().GetValue() == "" {
		err = errors.New("received update InfiniBand Partition request without ID")
	} else if request.GetConfig() == nil && request.GetMetadata() == nil {
		err = errors.New("received update InfiniBand Partition request without config or metadata")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	grpcClient := mibp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateIBPartition(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update InfiniBand Partition using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to delete InfiniBand Partition on NICo
func (mipb *ManageInfiniBandPartition) DeleteInfiniBandPartitionOnSite(ctx context.Context, request *corev1.IBPartitionDeletionRequest) error {
	logger := log.With().Str("Activity", "DeleteInfiniBandPartitionOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete InfiniBand Partition request")
	} else if request.Id == nil || request.Id.GetValue() == "" {
		err = errors.New("received delete InfiniBand Partition request without ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mipb.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteIBPartition(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete InfiniBand Partition using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}
