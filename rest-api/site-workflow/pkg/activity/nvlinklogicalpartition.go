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
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ManageNVLinkLogicalPartitionInventory is an activity wrapper for NVLinkLogical Partition inventory collection and publishing
type ManageNVLinkLogicalPartitionInventory struct {
	config ManageInventoryConfig
}

// DiscoverNVLinkLogicalPartitionInventory is an activity to collect NVLinkLogical Partition inventory and publish to Temporal queue
func (mmi *ManageNVLinkLogicalPartitionInventory) DiscoverNVLinkLogicalPartitionInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverNVLinkLogicalPartitionInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.NVLinkLogicalPartitionId, *corev1.NVLinkLogicalPartition, *corev1.NVLinkLogicalPartitionInventory]{
		itemType:               "NVLinkLogicalPartition",
		config:                 mmi.config,
		internalFindIDs:        nvllpFindIDs,
		internalFindByIDs:      nvllpFindByIDs,
		internalPagedInventory: nvllpPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageNVLinkLogicalPartitionInventory returns a ManageInventory implementation for NVLinkLogical Partition activity
func NewManageNVLinkLogicalPartitionInventory(config ManageInventoryConfig) ManageNVLinkLogicalPartitionInventory {
	return ManageNVLinkLogicalPartitionInventory{
		config: config,
	}
}

func nvllpFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.NVLinkLogicalPartitionId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	resp, err := grpcServiceClient.FindNVLinkLogicalPartitionIds(ctx, &corev1.NVLinkLogicalPartitionSearchFilter{})
	if err != nil {
		return nil, err
	}
	ids := make([]*corev1.NVLinkLogicalPartitionId, len(resp.GetPartitionIds()))
	for i, id := range resp.GetPartitionIds() {
		ids[i] = id
	}
	return ids, nil
}

func nvllpFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.NVLinkLogicalPartitionId) ([]*corev1.NVLinkLogicalPartition, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	req := &corev1.NVLinkLogicalPartitionsByIdsRequest{
		PartitionIds: ids,
	}
	resp, err := grpcServiceClient.FindNVLinkLogicalPartitionsByIds(ctx, req)
	if err != nil {
		return nil, err
	}
	return resp.GetPartitions(), nil
}

func nvllpPagedInventory(allItemIDs []*corev1.NVLinkLogicalPartitionId, pagedItems []*corev1.NVLinkLogicalPartition, input *pagedInventoryInput) *corev1.NVLinkLogicalPartitionInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of NVLinkLogicalPartitions
	inventory := &corev1.NVLinkLogicalPartitionInventory{
		Partitions: pagedItems,
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

// ManageNVLinkLogicalPartition is an activity wrapper for NVLinkLogical Partition management
type ManageNVLinkLogicalPartition struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// NewManageNVLinkLogicalPartition returns a new ManageNVLinkLogicalPartition client
func NewManageNVLinkLogicalPartition(coreGrpcClient *cClient.CoreGrpcAtomicClient) ManageNVLinkLogicalPartition {
	return ManageNVLinkLogicalPartition{
		coreGrpcAtomicClient: coreGrpcClient,
	}
}

// Function to create NVLinkLogical Partition with NICo
func (mnvllp *ManageNVLinkLogicalPartition) CreateNVLinkLogicalPartitionOnSite(ctx context.Context, request *corev1.NVLinkLogicalPartitionCreationRequest) (*corev1.NVLinkLogicalPartition, error) {
	logger := log.With().Str("Activity", "CreateNVLinkLogicalPartitionOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create NVLink Logical Partition request")
	} else if request.Id == nil || request.GetId().GetValue() == "" {
		err = errors.New("received create NVLink Logical Partition request missing ID")
	} else if request.Config == nil {
		err = errors.New("received create NVLink Logical Partition request missing Config")
	} else if request.Config.Metadata == nil {
		err = errors.New("received create NVLink Logical Partition request missing Metadata")
	} else if request.Config.Metadata.Name == "" {
		err = errors.New("received create NVLink Logical Partition request missing Name")
	} else if request.Config.TenantOrganizationId == "" {
		err = errors.New("received create NVLink Logical Partition request missing TenantOrganizationId")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mnvllp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call Core gRPC endpoint
	start := time.Now()
	nvLinkLogicalPartition, err := grpcServiceClient.CreateNVLinkLogicalPartition(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create NVLink Logical Partition using Core gRPC API")
		return nil, swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nvLinkLogicalPartition, nil
}

// Function to update NVLinkLogical Partition with NICo
func (mnvllp *ManageNVLinkLogicalPartition) UpdateNVLinkLogicalPartitionOnSite(ctx context.Context, request *corev1.NVLinkLogicalPartitionUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateNVLinkLogicalPartitionOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update NVLink Logical Partition request")
	} else if request.Id == nil || request.GetId().GetValue() == "" {
		err = errors.New("received update NVLink Logical Partition request missing ID")
	} else if request.Config == nil {
		err = errors.New("received update NVLink Logical Partition request missing Config")
	} else if request.Config.Metadata == nil {
		err = errors.New("received update NVLink Logical Partition request missing Metadata")
	} else if request.Config.Metadata.Name == "" {
		err = errors.New("received update NVLink Logical Partition request missing Name")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mnvllp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call Core gRPC endpoint
	start := time.Now()
	_, err = grpcServiceClient.UpdateNVLinkLogicalPartition(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update NVLink Logical Partition using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to delete NVLinkLogical Partition on NICo
func (mnvllp *ManageNVLinkLogicalPartition) DeleteNVLinkLogicalPartitionOnSite(ctx context.Context, request *corev1.NVLinkLogicalPartitionDeletionRequest) error {
	logger := log.With().Str("Activity", "DeleteNVLinkLogicalPartitionOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete NVLink Logical Partition request")
	} else if request.Id == nil || request.Id.GetValue() == "" {
		err = errors.New("received delete NVLink Logical Partition request without ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mnvllp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteNVLinkLogicalPartition(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete NVLink Logical Partition using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}
