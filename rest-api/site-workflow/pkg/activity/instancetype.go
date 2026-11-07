// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"time"

	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"

	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

// ManageInstanceType is an activity wrapper for InstanceType management tasks that allows injecting DB access
type ManageInstanceType struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// Function to Create NICo InstanceType with the Site Controller
func (mm *ManageInstanceType) CreateInstanceTypeOnSite(ctx context.Context, request *corev1.CreateInstanceTypeRequest) error {
	logger := log.With().Str("Activity", "CreateInstanceTypeOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create InstanceType request")
	} else if request.Id == nil || *request.Id == "" {
		err = errors.New("received create InstanceType request without ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.CreateInstanceType(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create InstanceType using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function Update NICo InstanceType with the Site Controller
func (mm *ManageInstanceType) UpdateInstanceTypeOnSite(ctx context.Context, request *corev1.UpdateInstanceTypeRequest) error {
	logger := log.With().Str("Activity", "UpdateInstanceTypeOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty InstanceType config update request")
	} else if request.Id == "" {
		err = errors.New("received InstanceType config update request without InstanceType ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateInstanceType(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update config for InstanceType using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Delete NICo InstanceType with the Site Controller
func (mm *ManageInstanceType) DeleteInstanceTypeOnSite(ctx context.Context, request *corev1.DeleteInstanceTypeRequest) error {
	logger := log.With().Str("Activity", "DeleteInstanceTypeOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete InstanceType request")
	} else if request.Id == "" {
		err = errors.New("received delete InstanceType request without InstanceType ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteInstanceType(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete InstanceType using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to associate Machine records with an InstanceType
func (mm *ManageInstanceType) AssociateMachinesWithInstanceTypeOnSite(ctx context.Context, request *corev1.AssociateMachinesWithInstanceTypeRequest) error {
	logger := log.With().Str("Activity", "AssociateMachinesWithInstanceTypeOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty AssociateMachinesWithInstanceTypeOnSite request")
	case request.InstanceTypeId == "":
		err = errors.New("received AssociateMachinesWithInstanceTypeOnSite without InstanceType ID")
	case len(request.MachineIds) == 0:
		err = errors.New("received AssociateMachinesWithInstanceTypeOnSite without Machine ID list")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.AssociateMachinesWithInstanceType(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to associate Machines with InstanceType using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to remove the association between a Machine and InstanceType
func (mm *ManageInstanceType) RemoveMachineInstanceTypeAssociationOnSite(ctx context.Context, request *corev1.RemoveMachineInstanceTypeAssociationRequest) error {
	logger := log.With().Str("Activity", "RemoveMachineInstanceTypeAssociationOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty RemoveMachineInstanceTypeAssociationOnSite request")
	case request.MachineId == "":
		err = errors.New("received RemoveMachineInstanceTypeAssociationOnSite request without Machine ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.RemoveMachineInstanceTypeAssociation(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to associate Machines with InstanceType using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// NewManageInstanceType returns a new ManageInstanceType activity
func NewManageInstanceType(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageInstanceType {
	return ManageInstanceType{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// ManageInstanceTypeInventory is an activity wrapper for InstanceType inventory collection and publishing
type ManageInstanceTypeInventory struct {
	config ManageInventoryConfig
}

// DiscoverInstanceTypeInventory is an activity to collect InstanceType inventory and publish to Temporal queue
func (mmi *ManageInstanceTypeInventory) DiscoverInstanceTypeInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverInstanceTypeInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.UUID, *corev1.InstanceType, *corev1.InstanceTypeInventory]{
		itemType:               "InstanceType",
		config:                 mmi.config,
		internalFindIDs:        instanceTypeFindIDs,
		internalFindByIDs:      instanceTypeFindByIDs,
		internalPagedInventory: instanceTypePagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageInstanceTypeInventory returns a ManageInventory implementation for InstanceType activity
func NewManageInstanceTypeInventory(config ManageInventoryConfig) ManageInstanceTypeInventory {
	return ManageInstanceTypeInventory{
		config: config,
	}
}

func instanceTypeFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.UUID, error) {
	// Call Core gRPC API endpoint
	grpcServiceClient := grpcClient.GrpcServiceClient()
	instanceTypeIdList, err := grpcServiceClient.FindInstanceTypeIds(ctx, &corev1.FindInstanceTypeIdsRequest{})
	if err != nil {
		return nil, err
	}
	return util.StringsToProtobufUUIDList(instanceTypeIdList.GetInstanceTypeIds()), nil
}

func instanceTypeFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.UUID) ([]*corev1.InstanceType, error) {
	// Call Core gRPC API endpoint
	grpcServiceClient := grpcClient.GrpcServiceClient()
	instanceTypeList, err := grpcServiceClient.FindInstanceTypesByIds(ctx, &corev1.FindInstanceTypesByIdsRequest{
		InstanceTypeIds: util.ProtobufUUIDListToStringList(ids),
	})
	if err != nil {
		return nil, err
	}
	return instanceTypeList.GetInstanceTypes(), nil
}

func instanceTypePagedInventory(allItemIDs []*corev1.UUID, pagedItems []*corev1.InstanceType, input *pagedInventoryInput) *corev1.InstanceTypeInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of Machines
	instanceTypeInventory := &corev1.InstanceTypeInventory{
		InstanceTypes: pagedItems,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if instanceTypeInventory.InventoryPage != nil {
		instanceTypeInventory.InventoryPage.ItemIds = itemIDs
	}
	return instanceTypeInventory
}
