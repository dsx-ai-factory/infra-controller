// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ManageSitePrefixInventory collects SitePrefix inventory from Core.
type ManageSitePrefixInventory struct {
	config ManageInventoryConfig
}

// NewManageSitePrefixInventory returns a SitePrefix inventory manager.
func NewManageSitePrefixInventory(config ManageInventoryConfig) ManageSitePrefixInventory {
	return ManageSitePrefixInventory{
		config: config,
	}
}

// DiscoverSitePrefixInventory collects every SitePrefix through Core's ID list and by-ID APIs.
func (mspi *ManageSitePrefixInventory) DiscoverSitePrefixInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverSitePrefixInventory").Logger()
	logger.Info().Msg("Starting activity")

	reportedAt := timestamppb.Now()
	inventoryImpl := manageInventoryImpl[*corev1.SitePrefixId, *corev1.SitePrefix, *corev1.SitePrefixInventory]{
		itemType:          "SitePrefix",
		config:            mspi.config,
		internalFindIDs:   sitePrefixFindIDs,
		internalFindByIDs: sitePrefixFindByIDs,
		internalPagedInventory: func(allItemIDs []*corev1.SitePrefixId, pagedItems []*corev1.SitePrefix,
			input *pagedInventoryInput) *corev1.SitePrefixInventory {
			return sitePrefixPagedInventory(allItemIDs, pagedItems, input, reportedAt)
		},
	}

	grpcClient := mspi.config.CoreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	buildInfo, err := grpcClient.GrpcServiceClient().Version(ctx, &corev1.VersionRequest{
		DisplayConfig: true,
	})
	if err != nil {
		inventoryImpl.newCollector(grpcClient).reportFailure(ctx, &logger, err)
		return fmt.Errorf("read Core max_find_by_ids for SitePrefix inventory: %w", err)
	}
	maxFindByIDs := buildInfo.GetRuntimeConfig().GetMaxFindByIds()
	if maxFindByIDs == 0 {
		// SitePrefix inventory has no legacy API fallback. Core enforces this value
		// as a maximum, so zero cannot authorize a nonempty by-ID request.
		err = errors.New("configured Core max_find_by_ids must be greater than zero for SitePrefix inventory")
		inventoryImpl.newCollector(grpcClient).reportFailure(ctx, &logger, err)
		return err
	}

	inventoryImpl.config.SitePageSize = min(max(1, inventoryImpl.config.SitePageSize), int(maxFindByIDs))
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// sitePrefixFindIDs returns the complete SitePrefix ID set reported by Core in ID order.
func sitePrefixFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.SitePrefixId, error) {
	idList, err := grpcClient.GrpcServiceClient().FindSitePrefixIds(ctx, &corev1.SitePrefixSearchFilter{})
	if err != nil {
		return nil, err
	}

	ids := slices.Clone(idList.GetSitePrefixIds())
	slices.SortFunc(ids, func(left, right *corev1.SitePrefixId) int {
		return strings.Compare(left.GetValue(), right.GetValue())
	})
	for index, id := range ids {
		value := id.GetValue()
		if value == "" {
			return nil, errors.New("received an empty SitePrefix ID from Core")
		}
		if index > 0 && ids[index-1].GetValue() == value {
			return nil, fmt.Errorf("received duplicate SitePrefix ID %q from Core", value)
		}
	}
	return ids, nil
}

// sitePrefixFindByIDs returns one exact SitePrefix request batch in ID order.
func sitePrefixFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.SitePrefixId) ([]*corev1.SitePrefix, error) {
	list, err := grpcClient.GrpcServiceClient().FindSitePrefixesByIds(ctx, &corev1.SitePrefixesByIdsRequest{
		SitePrefixIds: ids,
	})
	if err != nil {
		return nil, err
	}

	sitePrefixes := list.GetSitePrefixes()
	err = validateSitePrefixBatch(ids, sitePrefixes)
	if err != nil {
		return nil, err
	}
	sorted := slices.Clone(sitePrefixes)
	slices.SortFunc(sorted, func(left, right *corev1.SitePrefix) int {
		return strings.Compare(left.GetId().GetValue(), right.GetId().GetValue())
	})
	return sorted, nil
}

// validateSitePrefixBatch requires Core to return each requested SitePrefix exactly once.
// A complete run needs that exact match before a receiver can treat an ID as missing.
func validateSitePrefixBatch(requestedIDs []*corev1.SitePrefixId, sitePrefixes []*corev1.SitePrefix) error {
	requested := make(map[string]struct{}, len(requestedIDs))
	// sitePrefixFindIDs validates the complete ID set before it is split into batches.
	for _, id := range requestedIDs {
		requested[id.GetValue()] = struct{}{}
	}

	returned := make(map[string]struct{}, len(sitePrefixes))
	for _, sitePrefix := range sitePrefixes {
		value := sitePrefix.GetId().GetValue()
		_, exists := requested[value]
		if !exists {
			return fmt.Errorf("received Core response that returned unrequested SitePrefix ID %q", value)
		}
		_, exists = returned[value]
		if exists {
			return fmt.Errorf("received Core response that returned duplicate SitePrefix ID %q", value)
		}
		returned[value] = struct{}{}
	}

	for _, id := range requestedIDs {
		_, exists := returned[id.GetValue()]
		if !exists {
			return fmt.Errorf("received Core response that did not return requested SitePrefix ID %q", id.GetValue())
		}
	}
	return nil
}

// sitePrefixPagedInventory adds the run timestamp and complete ID set to one page.
func sitePrefixPagedInventory(allItemIDs []*corev1.SitePrefixId, pagedItems []*corev1.SitePrefix,
	input *pagedInventoryInput, reportedAt *timestamppb.Timestamp) *corev1.SitePrefixInventory {
	itemIDs := make([]string, 0, len(allItemIDs))
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	inventory := &corev1.SitePrefixInventory{
		SitePrefixes:    pagedItems,
		Timestamp:       reportedAt,
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if inventory.InventoryPage != nil {
		inventory.InventoryPage.ItemIds = itemIDs
	}
	return inventory
}
