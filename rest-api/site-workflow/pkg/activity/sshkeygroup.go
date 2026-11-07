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

// ManageSSHKeyGroupInventory is an activity wrapper for SSHKeyGroup inventory collection and publishing
type ManageSSHKeyGroupInventory struct {
	config ManageInventoryConfig
}

// DiscoverSSHKeyGroupInventory is an activity to collect SSHKeyGroup inventory and publish to Temporal queue
func (mmi *ManageSSHKeyGroupInventory) DiscoverSSHKeyGroupInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverSSHKeyGroupInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.TenantKeysetIdentifier, *corev1.TenantKeyset, *corev1.SSHKeyGroupInventory]{
		itemType:               "SSHKeyGroup",
		config:                 mmi.config,
		internalFindIDs:        sshKeyGroupFindIDs,
		internalFindByIDs:      sshKeyGroupFindByIDs,
		internalPagedInventory: sshKeyGroupPagedInventory,
		// A Cloud worker that predates the populated-list requirement reads this list on
		// every page, so withholding it there would read an absent SSH Key Group as removed
		// from the Site. Retire this once that requirement has rolled everywhere.
		itemIDsOnEveryPage: true,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageSSHKeyGroupInventory returns a ManageInventory implementation for SSHKeyGroup activity
func NewManageSSHKeyGroupInventory(config ManageInventoryConfig) ManageSSHKeyGroupInventory {
	return ManageSSHKeyGroupInventory{
		config: config,
	}
}

func sshKeyGroupFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.TenantKeysetIdentifier, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.FindTenantKeysetIds(ctx, &corev1.TenantKeysetSearchFilter{})
	if err != nil {
		return nil, err
	}
	return idList.GetKeysetIds(), nil
}

func sshKeyGroupFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.TenantKeysetIdentifier) ([]*corev1.TenantKeyset, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.FindTenantKeysetsByIds(ctx, &corev1.TenantKeysetsByIdsRequest{
		KeysetIds: ids,
	})
	if err != nil {
		return nil, err
	}
	return list.GetKeyset(), nil
}

func sshKeyGroupPagedInventory(allItemIDs []*corev1.TenantKeysetIdentifier, pagedItems []*corev1.TenantKeyset, input *pagedInventoryInput) *corev1.SSHKeyGroupInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetKeysetId())
	}

	// Create an inventory page
	inventory := &corev1.SSHKeyGroupInventory{
		TenantKeysets: pagedItems,
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

// ManageSSHKeyGroup is an activity wrapper for SSHKeyGroup management
type ManageSSHKeyGroup struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// NewManageSSHKeyGroup returns a new ManageSSHKeyGroup client
func NewManageSSHKeyGroup(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageSSHKeyGroup {
	return ManageSSHKeyGroup{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// Function to create SSH Key Group with NICo
func (mmi *ManageSSHKeyGroup) CreateSSHKeyGroupOnSite(ctx context.Context, request *corev1.CreateTenantKeysetRequest) error {
	logger := log.With().Str("Activity", "CreateSSHKeyGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create SSH Key Group request")
	} else if request.KeysetIdentifier == nil || request.GetKeysetIdentifier().GetKeysetId() == "" {
		err = errors.New("received create SSH Key Group request missing KeysetIdentifier")
	} else if request.KeysetIdentifier.OrganizationId == "" {
		err = errors.New("received create SSH Key Group request missing OrganizationId")
	} else if request.Version == "" {
		err = errors.New("received create SSH Key Group request missing Version")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mmi.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.CreateTenantKeyset(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create SSH Key Group using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Update SSH Key Group with NICo
func (mmi *ManageSSHKeyGroup) UpdateSSHKeyGroupOnSite(ctx context.Context, request *corev1.UpdateTenantKeysetRequest) error {
	logger := log.With().Str("Activity", "UpdateSSHKeyGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update SSH Key Group request")
	} else if request.KeysetIdentifier == nil || request.GetKeysetIdentifier().GetKeysetId() == "" {
		err = errors.New("received update SSH Key Group request missing KeysetIdentifier")
	} else if request.KeysetIdentifier.OrganizationId == "" {
		err = errors.New("received update SSH Key Group request missing OrganizationId")
	} else if request.Version == "" {
		err = errors.New("received update SSH Key Group request missing Version")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mmi.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateTenantKeyset(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update SSH Key Group using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Delete SSH Key Group with NICo
func (mmi *ManageSSHKeyGroup) DeleteSSHKeyGroupOnSite(ctx context.Context, request *corev1.DeleteTenantKeysetRequest) error {
	logger := log.With().Str("Activity", "DeleteSSHKeyGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete SSH Key Group request")
	} else if request.KeysetIdentifier == nil || request.GetKeysetIdentifier().GetKeysetId() == "" {
		err = errors.New("received delete SSH Key Group request missing KeysetIdentifier")
	} else if request.KeysetIdentifier.OrganizationId == "" {
		err = errors.New("received delete SSH Key Group request missing OrganizationId")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mmi.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteTenantKeyset(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete SSH Key Group using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}
