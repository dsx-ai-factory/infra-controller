// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	cclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/timestamppb"
)

type ManageDpuExtensionServiceInventory struct {
	config ManageInventoryConfig
}

// DiscoverDpuExtensionServiceInventory is an activity to discover DPU Extension Services on Site and publish to Temporal queue
func (msi *ManageDpuExtensionServiceInventory) DiscoverDpuExtensionServiceInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverDpuExtensionServiceInventory").Logger()

	logger.Info().Msg("Starting activity")

	inventoryImpl := manageInventoryImpl[string, *corev1.DpuExtensionService, *corev1.DpuExtensionServiceInventory]{
		itemType:               "DpuExtensionService",
		config:                 msi.config,
		internalFindIDs:        dpuExtensionServiceFindIDs,
		internalFindByIDs:      dpuExtensionServiceFindByIDs,
		internalPagedInventory: dpuExtensionServicePagedInventory,
	}

	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageDpuExtensionServiceInventory returns a ManageInventory implementation for DPU Extension Service activity
func NewManageDpuExtensionServiceInventory(config ManageInventoryConfig) ManageDpuExtensionServiceInventory {
	return ManageDpuExtensionServiceInventory{
		config: config,
	}
}

func dpuExtensionServiceFindIDs(ctx context.Context, grpcClient *cclient.CoreGrpcClient) ([]string, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	result, err := grpcServiceClient.FindDpuExtensionServiceIds(ctx, &corev1.DpuExtensionServiceSearchFilter{})
	if err != nil {
		return nil, err
	}

	return result.ServiceIds, nil
}

func dpuExtensionServiceFindByIDs(ctx context.Context, grpcClient *cclient.CoreGrpcClient, ids []string) ([]*corev1.DpuExtensionService, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	result, err := grpcServiceClient.FindDpuExtensionServicesByIds(ctx, &corev1.DpuExtensionServicesByIdsRequest{
		ServiceIds: ids,
	})
	if err != nil {
		return nil, err
	}
	return result.Services, nil
}

func dpuExtensionServicePagedInventory(allItemIDs []string, pagedItems []*corev1.DpuExtensionService, input *pagedInventoryInput) *corev1.DpuExtensionServiceInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id)
	}

	inventory := &corev1.DpuExtensionServiceInventory{
		DpuExtensionServices: pagedItems,
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

// ManageDpuExtensionService is an activity wrapper for DPU Extension Service management
type ManageDpuExtensionService struct {
	coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient
}

// CreateDpuExtensionServiceOnSite is an activity to create a new DPU Extension Service on Site
func (mdes *ManageDpuExtensionService) CreateDpuExtensionServiceOnSite(ctx context.Context, request *corev1.CreateDpuExtensionServiceRequest) (*corev1.DpuExtensionService, error) {
	logger := log.With().Str("Activity", "CreateDpuExtensionServiceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create DPU Extension Service request")
	} else if request.ServiceId == nil || *request.ServiceId == "" {
		err = errors.New("received create DPU Extension Service request without ID")
	} else if request.ServiceName == "" {
		err = errors.New("received create DPU Extension Service request without name")
	} else if request.TenantOrganizationId == "" {
		err = errors.New("received create DPU Extension Service request without tenant organization ID")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mdes.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	createdDpuExtensionService, err := grpcServiceClient.CreateDpuExtensionService(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create DPU Extension Service using Core gRPC API")
		return nil, swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return createdDpuExtensionService, nil
}

// UpdateDpuExtensionServiceOnSite is an activity to update a DPU Extension Service on Site
func (mdes *ManageDpuExtensionService) UpdateDpuExtensionServiceOnSite(ctx context.Context, request *corev1.UpdateDpuExtensionServiceRequest) (*corev1.DpuExtensionService, error) {
	logger := log.With().Str("Activity", "UpdateDpuExtensionServiceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update DPU Extension Service request")
	} else if request.ServiceId == "" {
		err = errors.New("received update DPU Extension Service request without ID")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mdes.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	updatedDpuExtensionService, err := grpcServiceClient.UpdateDpuExtensionService(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update DPU Extension Service using Core gRPC API")
		return nil, swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return updatedDpuExtensionService, nil
}

// DeleteDpuExtensionServiceOnSite is an activity to delete a DPU Extension Service on Site
func (mdes *ManageDpuExtensionService) DeleteDpuExtensionServiceOnSite(ctx context.Context, request *corev1.DeleteDpuExtensionServiceRequest) error {
	logger := log.With().Str("Activity", "DeleteDpuExtensionServiceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete DPU Extension Service request")
	} else if request.ServiceId == "" {
		err = errors.New("received delete DPU Extension Service request without ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mdes.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteDpuExtensionService(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete DPU Extension Service using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// GetDpuExtensionServiceVersionsInfoOnSite is an activity to get detailed information for various versions of a DPU Extension Service on Site
func (mdes *ManageDpuExtensionService) GetDpuExtensionServiceVersionsInfoOnSite(ctx context.Context, request *corev1.GetDpuExtensionServiceVersionsInfoRequest) (*corev1.DpuExtensionServiceVersionInfoList, error) {
	logger := log.With().Str("Activity", "GetDpuExtensionServiceVersionsInfoOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty get DPU Extension Service versions info request")
	} else if request.ServiceId == "" {
		err = errors.New("received get DPU Extension Service versions info request without ID")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mdes.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	versionInfos, err := grpcServiceClient.GetDpuExtensionServiceVersionsInfo(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get DPU Extension Service versions info using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return versionInfos, nil
}

// NewManageDpuExtensionService returns a new ManageDpuExtensionService activity
func NewManageDpuExtensionService(coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient) ManageDpuExtensionService {
	return ManageDpuExtensionService{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}
