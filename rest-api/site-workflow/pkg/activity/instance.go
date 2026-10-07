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
)

// ManageInstance is an activity wrapper for Instance management tasks that allows injecting DB access
type ManageInstance struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// Function Update NICo Instance with the Site Controller
func (mm *ManageInstance) UpdateInstanceOnSite(ctx context.Context, request *corev1.InstanceConfigUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateInstanceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty Instance config update request")
	} else if request.InstanceId == nil {
		err = errors.New("received Instance config update request without Instance ID")
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
	_, err = grpcServiceClient.UpdateInstanceConfig(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update config for Instance using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Create (allocate) NICo Instance with the Site Controller
func (mm *ManageInstance) CreateInstanceOnSite(ctx context.Context, request *corev1.InstanceAllocationRequest) error {
	logger := log.With().Str("Activity", "CreateInstanceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create Instance request")
	} else if request.MachineId == nil {
		err = errors.New("received create Instance request without Machine ID")
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
	_, err = grpcServiceClient.AllocateInstance(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Instance using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// CreateInstancesOnSite is an activity to create (allocate) multiple NICo Instances with the Site Controller
// in a single transaction. This is the batch version of CreateInstanceOnSite.
func (mm *ManageInstance) CreateInstancesOnSite(ctx context.Context, request *corev1.BatchInstanceAllocationRequest) error {
	logger := log.With().Str("Activity", "CreateInstancesOnSite").Logger()

	var err error
	if request == nil {
		err = errors.New("received empty batch create Instance request")
	} else if len(request.InstanceRequests) == 0 {
		err = errors.New("received batch create Instance request with no instances")
	}
	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	logger = log.With().Str("Activity", "CreateInstancesOnSite").Int("Count", len(request.InstanceRequests)).Logger()
	logger.Info().Msg("Starting batch instance allocation activity")

	for i, req := range request.InstanceRequests {
		if req.MachineId == nil {
			err = errors.New("received create Instance request without Machine ID at index " + string(rune(i)))
			return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
		}
	}

	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.AllocateInstances(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to batch create Instances using Core gRPC API")
		return swe.WrapErr(err)
	}

	logger.Info().Int("Count", len(request.InstanceRequests)).Dur("grpc_duration", duration).Msg("Completed batch instance allocation activity")
	return nil
}

// Function to Create (allocate) NICo Instance with the Site Controller
func (mm *ManageInstance) RebootInstanceOnSite(ctx context.Context, request *corev1.InstancePowerRequest) error {
	logger := log.With().Str("Activity", "RebootInstanceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty reboot Instance request")
	} else if request.InstanceId == nil {
		err = errors.New("received reboot Instance request without Instance ID")
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
	_, err = grpcServiceClient.InvokeInstancePower(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to reboot Instance using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to Delete NICo Instance with the Site Controller
func (mm *ManageInstance) DeleteInstanceOnSite(ctx context.Context, request *corev1.InstanceReleaseRequest) error {
	logger := log.With().Str("Activity", "DeleteInstanceOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete Instance request")
	} else if request.Id == nil || request.Id.Value == "" {
		err = errors.New("received delete Instance request without Instance ID")
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
	_, err = grpcServiceClient.ReleaseInstance(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete Instance using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// NewManageInstance returns a new ManageInstance activity
func NewManageInstance(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageInstance {
	return ManageInstance{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// ManageInstanceInventory is an activity wrapper for Instance inventory collection and publishing
type ManageInstanceInventory struct {
	config ManageInventoryConfig
}

// DiscoverInstanceInventory is an activity to collect Instance inventory and publish to Temporal queue
func (mmi *ManageInstanceInventory) DiscoverInstanceInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverInstanceInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.InstanceId, *corev1.Instance, *corev1.InstanceInventory]{
		itemType:                          "Instance",
		config:                            mmi.config,
		internalFindIDs:                   instanceFindIDs,
		internalFindByIDs:                 instanceFindByIDs,
		internalPagedInventory:            instancePagedInventory,
		internalPagedInventoryPostProcess: instancePagedInventoryPostProcess,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageInstanceInventory returns a ManageInventory implementation for Instance activity
func NewManageInstanceInventory(config ManageInventoryConfig) ManageInstanceInventory {
	return ManageInstanceInventory{
		config: config,
	}
}

func instanceFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.InstanceId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	instanceIdList, err := grpcServiceClient.FindInstanceIds(ctx, &corev1.InstanceSearchFilter{})
	if err != nil {
		return nil, err
	}
	return instanceIdList.GetInstanceIds(), nil
}

func instanceFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.InstanceId) ([]*corev1.Instance, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	instanceList, err := grpcServiceClient.FindInstancesByIds(ctx, &corev1.InstancesByIdsRequest{
		InstanceIds: ids,
	})
	if err != nil {
		return nil, err
	}

	return instanceList.GetInstances(), nil
}

// instancePagedInventoryPostProcess will attach NSG propagation information for the inventory page of instances.
// This will only be called for pages with inventory.
func instancePagedInventoryPostProcess(ctx context.Context, grpcClient *cClient.CoreGrpcClient, inventory *corev1.InstanceInventory) (*corev1.InstanceInventory, error) {
	instanceIds := make([]string, len(inventory.GetInstances()))

	for i, instance := range inventory.GetInstances() {
		instanceIds[i] = instance.GetId().GetValue()
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()
	propList, err := grpcServiceClient.GetNetworkSecurityGroupPropagationStatus(ctx, &corev1.GetNetworkSecurityGroupPropagationStatusRequest{
		InstanceIds: instanceIds,
	})

	if err != nil {
		return nil, err
	}

	inventory.NetworkSecurityGroupPropagations = propList.GetInstances()

	return inventory, nil
}

func instancePagedInventory(allItemIDs []*corev1.InstanceId, pagedItems []*corev1.Instance, input *pagedInventoryInput) *corev1.InstanceInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of Machines
	instanceInventory := &corev1.InstanceInventory{
		Instances: pagedItems,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if instanceInventory.InventoryPage != nil {
		instanceInventory.InventoryPage.ItemIds = itemIDs
	}
	return instanceInventory
}
