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

	gcodes "google.golang.org/grpc/codes"
	gstatus "google.golang.org/grpc/status"
)

// ManageIpxeTemplateInventory is an activity wrapper for iPXE template inventory collection
// and publishing (inbound pull path: nico-core -> cloud).
type ManageIpxeTemplateInventory struct {
	config ManageInventoryConfig
}

// NewManageIpxeTemplateInventory returns a ManageIpxeTemplateInventory activity
func NewManageIpxeTemplateInventory(config ManageInventoryConfig) ManageIpxeTemplateInventory {
	return ManageIpxeTemplateInventory{
		config: config,
	}
}

// DiscoverIpxeTemplateInventory collects iPXE template inventory from the Site Controller
// and publishes it to the cloud Temporal queue. Only PUBLIC templates are propagated to
// REST (core is the source of truth; one-way sync).
//
// It uses the shared paged inventory pipeline (see manageInventoryImpl). The Site Controller
// exposes only a list-style API for iPXE templates (no find-by-ID), so collection goes
// through the fallback path, and the full reported ID set travels in the final page's
// InventoryPage.ItemIds for the cloud reconciler to detect deletions.
func (mii *ManageIpxeTemplateInventory) DiscoverIpxeTemplateInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverIpxeTemplateInventory").Logger()
	logger.Info().Msg("Starting activity")

	inventoryImpl := manageInventoryImpl[*corev1.IpxeTemplateId, *corev1.IpxeTemplate, *corev1.IpxeTemplateInventory]{
		itemType:               "IpxeTemplate",
		config:                 mii.config,
		internalFindIDs:        ipxeTemplateFindIDs,
		internalFindByIDs:      ipxeTemplateFindByIDs,
		internalPagedInventory: ipxeTemplatePagedInventory,
		internalFindFallback:   ipxeTemplateFindFallback,
	}

	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// iPXE templates have no find-by-ID API on the Site Controller, so the ID-based
// collection path is intentionally unimplemented to route through the fallback.
func ipxeTemplateFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.IpxeTemplateId, error) {
	return nil, gstatus.Error(gcodes.Unimplemented, "")
}

func ipxeTemplateFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.IpxeTemplateId) ([]*corev1.IpxeTemplate, error) {
	return nil, gstatus.Error(gcodes.Unimplemented, "")
}

func ipxeTemplatePagedInventory(allItemIDs []*corev1.IpxeTemplateId, pagedItems []*corev1.IpxeTemplate, input *pagedInventoryInput) *corev1.IpxeTemplateInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	inventory := &corev1.IpxeTemplateInventory{
		Templates: pagedItems,
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

// ipxeTemplateFindFallback lists iPXE templates from the Site Controller and returns only
// PUBLIC ones (core is the source of truth; one-way sync). Both the returned IDs and the
// returned templates are filtered so the full reported ID set carried in InventoryPage.ItemIds
// contains only PUBLIC templates.
func ipxeTemplateFindFallback(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.IpxeTemplateId, []*corev1.IpxeTemplate, error) {
	result, err := grpcClient.GrpcServiceClient().ListIpxeTemplates(ctx, &corev1.ListIpxeTemplatesRequest{})
	if err != nil {
		return nil, nil, err
	}

	var ids []*corev1.IpxeTemplateId
	var templates []*corev1.IpxeTemplate
	for _, t := range result.GetTemplates() {
		if t.GetVisibility() == corev1.IpxeTemplateVisibility_PUBLIC {
			templates = append(templates, t)
			ids = append(ids, t.GetId())
		}
	}

	return ids, templates, nil
}
