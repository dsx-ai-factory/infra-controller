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

// ManageVPC is an activity wrapper for VPC management
// TODO: Do we really need a distinction between general management and inventory?
// The pattern is elsewhere as well, but it seems like we could condense them since
// Manage*Inventory.config has a property that holds a *client.NICoCoreAtomicClient.
type ManageVPC struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// ManageVPCInventory is an activity wrapper for VPC inventory collection and publishing
type ManageVPCInventory struct {
	config ManageInventoryConfig
}

// NewManageVPC returns a new ManageVPC client
func NewManageVPC(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageVPC {
	return ManageVPC{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// DiscoverVPCInventory is an activity to collect VPC inventory and publish to Temporal queue
func (mvi *ManageVPCInventory) DiscoverVPCInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverVPCInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.VpcId, *corev1.Vpc, *corev1.VPCInventory]{
		itemType:                          "Vpc",
		config:                            mvi.config,
		internalFindIDs:                   vpcFindIDs,
		internalFindByIDs:                 vpcFindByIDs,
		internalPagedInventory:            vpcPagedInventory,
		internalPagedInventoryPostProcess: vpcPagedInventoryPostProcess,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageVPCInventory returns a ManageInventory implementation for VPC activity
func NewManageVPCInventory(config ManageInventoryConfig) ManageVPCInventory {
	return ManageVPCInventory{
		config: config,
	}
}

func vpcFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.VpcId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.FindVpcIds(ctx, &corev1.VpcSearchFilter{})
	if err != nil {
		return nil, err
	}
	return idList.GetVpcIds(), nil
}

func vpcFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.VpcId) ([]*corev1.Vpc, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.FindVpcsByIds(ctx, &corev1.VpcsByIdsRequest{
		VpcIds: ids,
	})
	if err != nil {
		return nil, err
	}

	return list.GetVpcs(), nil
}

// instancePagedInventoryPostProcess will attach NSG propagation
// information for the inventory page of VPCs.
// This will only be called for pages with inventory.
func vpcPagedInventoryPostProcess(ctx context.Context, grpcClient *cClient.CoreGrpcClient, inventory *corev1.VPCInventory) (*corev1.VPCInventory, error) {
	vpcIds := make([]string, len(inventory.GetVpcs()))

	for i, vpc := range inventory.GetVpcs() {
		vpcIds[i] = vpc.GetId().GetValue()
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()
	propList, err := grpcServiceClient.GetNetworkSecurityGroupPropagationStatus(ctx, &corev1.GetNetworkSecurityGroupPropagationStatusRequest{
		VpcIds: vpcIds,
	})

	if err != nil {
		return nil, err
	}

	inventory.NetworkSecurityGroupPropagations = propList.GetVpcs()

	return inventory, nil
}

func vpcPagedInventory(allItemIDs []*corev1.VpcId, pagedItems []*corev1.Vpc, input *pagedInventoryInput) *corev1.VPCInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetValue())
	}

	// Create an inventory page with the subset of VPCs
	inventory := &corev1.VPCInventory{
		Vpcs: pagedItems,
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

// Function to create VPCS with NICo
func (mv *ManageVPC) CreateVpcOnSite(ctx context.Context, request *corev1.VpcCreationRequest) (*corev1.Vpc, error) {
	logger := log.With().Str("Activity", "CreateVpcOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty create VPC request")
	case request.Name == "":
		err = errors.New("received create VPC request missing name")
	case request.TenantOrganizationId == "":
		err = errors.New("received create VPC request missing TenantOrganizationId")
	case request.Id == nil || request.Id.Value == "":
		// Don't let a request come in without a cloud-provided ID
		// or nico will generate one and cloud won't know the relationship.
		err = errors.New("received create VPC request missing VPC ID")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	controllerVpc, err := grpcServiceClient.CreateVpc(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create VPC using Core gRPC API")
		return nil, swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return controllerVpc, nil
}

// Function to update VPCS with NICo
func (mv *ManageVPC) UpdateVpcOnSite(ctx context.Context, request *corev1.VpcUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateVpcOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty update VPC request")
	case request.Id == nil || request.Id.Value == "":
		// Don't let a request come in without a cloud-provided ID
		// or nico will generate one and cloud won't know the relationship.
		err = errors.New("received update VPC request missing VPC ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateVpc(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update VPC using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// Function to delete VPCS with NICo
func (mv *ManageVPC) DeleteVpcOnSite(ctx context.Context, request *corev1.VpcDeletionRequest) error {
	logger := log.With().Str("Activity", "DeleteVpcOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty delete VPC request")
	case request.Id == nil || request.Id.Value == "":

		err = errors.New("received delete VPC request missing VPC ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteVpc(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete VPC using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// UpdateVpcVirtualizationOnSite updates VPC virtualization on Site
func (mv *ManageVPC) UpdateVpcVirtualizationOnSite(ctx context.Context, request *corev1.VpcUpdateVirtualizationRequest) error {
	logger := log.With().Str("Activity", "UpdateVpcOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	switch {
	case request == nil:
		err = errors.New("received empty update VPC virtualization request")
	case request.Id == nil || request.Id.Value == "":
		err = errors.New("received update VPC virtualization request missing VPC ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateVpcVirtualization(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update VPC virtualization using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}
