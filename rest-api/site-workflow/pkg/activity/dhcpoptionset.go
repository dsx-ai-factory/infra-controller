// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"time"

	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// ManageDhcpOptionSetInventory is an activity wrapper for DhcpOptionSet inventory collection and publishing.
// DhcpOptionSet create, update and delete reach Core through the generic gRPC proxy, so there are no CRUD activities.
type ManageDhcpOptionSetInventory struct {
	config ManageInventoryConfig
}

// DiscoverDhcpOptionSetInventory is an activity to collect DhcpOptionSet inventory and publish to Temporal queue
func (mdi *ManageDhcpOptionSetInventory) DiscoverDhcpOptionSetInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverDhcpOptionSetInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.DhcpOptionSetId, *corev1.DhcpOptionSet, *corev1.DhcpOptionSetInventory]{
		itemType:               "DhcpOptionSet",
		config:                 mdi.config,
		internalFindIDs:        dhcpOptionSetFindIDs,
		internalFindByIDs:      dhcpOptionSetFindByIDs,
		internalPagedInventory: dhcpOptionSetPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageDhcpOptionSetInventory returns a ManageInventory implementation for DhcpOptionSet activity
func NewManageDhcpOptionSetInventory(config ManageInventoryConfig) ManageDhcpOptionSetInventory {
	return ManageDhcpOptionSetInventory{
		config: config,
	}
}

func dhcpOptionSetFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.DhcpOptionSetId, error) {
	resp, err := grpcClient.GrpcServiceClient().FindDhcpOptionSetIds(ctx, &corev1.FindDhcpOptionSetIdsRequest{})
	if err != nil {
		return nil, err
	}
	return resp.GetIds(), nil
}

func dhcpOptionSetFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.DhcpOptionSetId) ([]*corev1.DhcpOptionSet, error) {
	resp, err := grpcClient.GrpcServiceClient().FindDhcpOptionSetsByIds(ctx, &corev1.FindDhcpOptionSetsByIdsRequest{
		Ids: ids,
	})
	if err != nil {
		return nil, err
	}
	return resp.GetDhcpOptionSets(), nil
}

func dhcpOptionSetPagedInventory(allItemIDs []*corev1.DhcpOptionSetId, pagedItems []*corev1.DhcpOptionSet, input *pagedInventoryInput) *corev1.DhcpOptionSetInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	inventory := &corev1.DhcpOptionSetInventory{
		DhcpOptionSets: pagedItems,
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
