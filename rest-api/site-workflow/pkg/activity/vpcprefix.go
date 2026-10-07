// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ManageVpcPrefix is an activity wrapper for VpcPrefix management
type ManageVpcPrefix struct {
	coreGrpcAtomicClient *client.CoreGrpcAtomicClient
}

// NewManageVpcPrefix returns a new ManageVpcPrefix client
func NewManageVpcPrefix(coreGrpcAtomicClient *client.CoreGrpcAtomicClient) ManageVpcPrefix {
	return ManageVpcPrefix{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// Function to create VpcPrefix with NICo
func (mvp *ManageVpcPrefix) CreateVpcPrefixOnSite(ctx context.Context, request *corev1.VpcPrefixCreationRequest) error {
	logger := log.With().Str("Activity", "CreateVpcPrefixOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create VPC Prefix request")
	} else if request.Id == nil || request.Id.Value == "" {
		// Don't let a request come in without a cloud-provided ID
		// or nico will generate one and cloud won't know the relationship.
		err = errors.New("received create VPC prefix request missing ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mvp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.CreateVpcPrefix(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create VPC Prefix using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to update VpcPrefix with NICo
func (mvp *ManageVpcPrefix) UpdateVpcPrefixOnSite(ctx context.Context, request *corev1.VpcPrefixUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateVpcPrefixOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update VPC Prefix request")
	} else if request.Id == nil || request.Id.Value == "" {
		err = errors.New("received update VPC Prefix request without ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mvp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateVpcPrefix(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update VPC Prefix using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to delete VpcPrefix on NICo
func (mvp *ManageVpcPrefix) DeleteVpcPrefixOnSite(ctx context.Context, request *corev1.VpcPrefixDeletionRequest) error {
	logger := log.With().Str("Activity", "DeleteVpcPrefixOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete VPC Prefix request")
	} else if request.Id == nil || request.Id.Value == "" {
		err = errors.New("received delete VPC Prefix request without ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mvp.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteVpcPrefix(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete VPC Prefix using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// ManageVpcPrefixInventory is an activity wrapper for VpcPrefix inventory collection and publishing
type ManageVpcPrefixInventory struct {
	config ManageInventoryConfig
}

// NewManageVpcPrefixInventory returns a ManageInventory implementation for VpcPrefix
func NewManageVpcPrefixInventory(config ManageInventoryConfig) ManageVpcPrefixInventory {
	return ManageVpcPrefixInventory{
		config: config,
	}
}

// DiscoverVpcPrefixInventory is an activity to collect VpcPrefix inventory and publish to Temporal queue
func (mvpi *ManageVpcPrefixInventory) DiscoverVpcPrefixInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverVpcPrefixInventory").Logger()
	logger.Info().Msg("Starting activity")

	inventoryImpl := manageInventoryImpl[*corev1.VpcPrefixId, *corev1.VpcPrefix, *corev1.VpcPrefixInventory]{
		itemType:               "VpcPrefix",
		config:                 mvpi.config,
		internalFindIDs:        vpcPrefixFindIDs,
		internalFindByIDs:      vpcPrefixFindByIDs,
		internalPagedInventory: vpcPrefixPagedInventory,
		internalFindFallback:   vpcPrefixFindFallback,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

func vpcPrefixFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.VpcPrefixId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.SearchVpcPrefixes(ctx, &corev1.VpcPrefixSearchQuery{
		Deleted: corev1.DeletedFilter_DELETED_FILTER_INCLUDE,
	})
	if err != nil {
		return nil, err
	}
	return idList.VpcPrefixIds, nil
}

func vpcPrefixFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.VpcPrefixId) ([]*corev1.VpcPrefix, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.GetVpcPrefixes(ctx, &corev1.VpcPrefixGetRequest{
		VpcPrefixIds: ids,
		Deleted:      corev1.DeletedFilter_DELETED_FILTER_INCLUDE,
	})

	if err != nil {
		return nil, err
	}
	return list.GetVpcPrefixes(), nil
}

func vpcPrefixPagedInventory(allItemIDs []*corev1.VpcPrefixId, pagedItems []*corev1.VpcPrefix, input *pagedInventoryInput) *corev1.VpcPrefixInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of VpcPrefixs
	inventory := &corev1.VpcPrefixInventory{
		VpcPrefixes: pagedItems,
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

func vpcPrefixFindFallback(ctx context.Context, coreGrpcClient *cClient.CoreGrpcClient) ([]*corev1.VpcPrefixId, []*corev1.VpcPrefix, error) {
	grpcServiceClient := coreGrpcClient.GrpcServiceClient()
	request := &corev1.VpcPrefixGetRequest{
		Deleted: corev1.DeletedFilter_DELETED_FILTER_INCLUDE,
	}
	items, err := grpcServiceClient.GetVpcPrefixes(ctx, request)
	if err != nil {
		return nil, nil, err
	}

	var ids []*corev1.VpcPrefixId
	for _, it := range items.GetVpcPrefixes() {
		ids = append(ids, it.GetId())
	}
	return ids, items.GetVpcPrefixes(), nil
}
