// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ManageSpectrumXPartitionInventory is an activity wrapper for SpectrumX Partition inventory collection and publishing
type ManageSpectrumXPartitionInventory struct {
	config ManageInventoryConfig
}

// DiscoverSpectrumXPartitionInventory is an activity to collect SpectrumX Partition inventory and publish to Temporal queue.
//
// The itemType is what names the Cloud workflow the collector publishes to, so it has to
// stay "SpectrumXPartition" to reach UpdateSpectrumXPartitionInventory.
func (mmi *ManageSpectrumXPartitionInventory) DiscoverSpectrumXPartitionInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverSpectrumXPartitionInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.SpxPartitionId, *corev1.SpxPartition, *corev1.SpectrumXPartitionInventory]{
		itemType:               "SpectrumXPartition",
		config:                 mmi.config,
		internalFindIDs:        spxpFindIDs,
		internalFindByIDs:      spxpFindByIDs,
		internalPagedInventory: spxpPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageSpectrumXPartitionInventory returns a ManageInventory implementation for SpectrumX Partition activity
func NewManageSpectrumXPartitionInventory(config ManageInventoryConfig) ManageSpectrumXPartitionInventory {
	return ManageSpectrumXPartitionInventory{
		config: config,
	}
}

func spxpFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.SpxPartitionId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.FindSpxPartitionIds(ctx, &corev1.SpxPartitionSearchFilter{})
	if err != nil {
		return nil, err
	}
	return idList.GetSpxPartitionIds(), nil
}

func spxpFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.SpxPartitionId) ([]*corev1.SpxPartition, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.FindSpxPartitionsByIds(ctx, &corev1.SpxPartitionsByIdsRequest{
		SpxPartitionIds: ids,
	})
	if err != nil {
		return nil, err
	}
	return list.GetSpxPartitions(), nil
}

func spxpPagedInventory(allItemIDs []*corev1.SpxPartitionId, pagedItems []*corev1.SpxPartition, input *pagedInventoryInput) *corev1.SpectrumXPartitionInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Every page carries the full ID set so Cloud can detect Partitions that
	// disappeared from the Site.
	inventory := &corev1.SpectrumXPartitionInventory{
		SpxPartitions: pagedItems,
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
