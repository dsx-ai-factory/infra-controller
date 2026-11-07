// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ManageSkuInventory is an activity wrapper for Sku inventory collection and publishing
type ManageSkuInventory struct {
	config ManageInventoryConfig
}

// DiscoverSkuInventory is an activity to collect Sku inventory and publish to Temporal queue
func (msi *ManageSkuInventory) DiscoverSkuInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverSkuInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[string, *corev1.Sku, *corev1.SkuInventory]{
		itemType:               "Sku",
		config:                 msi.config,
		internalFindIDs:        skuFindIDs,
		internalFindByIDs:      skuFindByIDs,
		internalPagedInventory: skuPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageSkuInventory returns a ManageInventory implementation for Sku activity
func NewManageSkuInventory(config ManageInventoryConfig) ManageSkuInventory {
	return ManageSkuInventory{
		config: config,
	}
}

func skuFindIDs(ctx context.Context, grpcClient *cclient.CoreGrpcClient) ([]string, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	result, err := grpcServiceClient.GetAllSkuIds(ctx, nil)
	if err != nil {
		return nil, err
	}

	ids := []string{}
	for _, id := range result.Ids {
		cid := id
		ids = append(ids, cid)
	}

	return ids, nil
}

func skuFindByIDs(ctx context.Context, grpcClient *cclient.CoreGrpcClient, ids []string) ([]*corev1.Sku, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	result, err := grpcServiceClient.FindSkusByIds(ctx, &corev1.SkusByIdsRequest{
		Ids: ids,
	})
	if err != nil {
		return nil, err
	}

	return result.Skus, nil
}

func skuPagedInventory(ids []string, skus []*corev1.Sku, input *pagedInventoryInput) *corev1.SkuInventory {
	// Create an inventory page
	inventory := &corev1.SkuInventory{
		Skus: skus,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if inventory.InventoryPage != nil {
		inventory.InventoryPage.ItemIds = ids
	}
	return inventory
}
