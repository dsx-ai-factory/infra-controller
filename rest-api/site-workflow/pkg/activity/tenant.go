// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"time"

	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"

	"go.temporal.io/sdk/temporal"

	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageTenant is activity to manage a Tenant on Site
type ManageTenant struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// CreateTenantOnSite creates a Tenant by calling Site Controller gRPC API
func (mt *ManageTenant) CreateTenantOnSite(ctx context.Context, request *corev1.CreateTenantRequest) error {
	logger := log.With().Str("Activity", "CreateTenantOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty Tenant request")
	} else if request.OrganizationId == "" {
		err = errors.New("received Tenant creation request without Organization ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mt.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.CreateTenant(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Tenant using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return err
}

// UpdateTenantOnSite creates a Tenant by calling Site Controller gRPC API
func (mt *ManageTenant) UpdateTenantOnSite(ctx context.Context, request *corev1.UpdateTenantRequest) error {
	logger := log.With().Str("Activity", "UpdateTenantOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty Tenant request")
	} else if request.OrganizationId == "" {
		err = errors.New("received Tenant update request without Organization ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mt.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateTenant(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update Tenant using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return err
}

// NewManageTenant returns a new ManageTenant activity
func NewManageTenant(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageTenant {
	return ManageTenant{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// ManageTenantInventory is an activity wrapper for VPC inventory collection and publishing
type ManageTenantInventory struct {
	config ManageInventoryConfig
}

// ManageTenantInventory is an activity to collect Tenant inventory and publish to Cloud
func (mti *ManageTenantInventory) DiscoverTenantInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverTenantInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[string, *corev1.Tenant, *corev1.TenantInventory]{
		itemType:               "Tenant",
		config:                 mti.config,
		internalFindIDs:        tenantFindIDs,
		internalFindByIDs:      tenantFindByIDs,
		internalPagedInventory: tenantPagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageTenantInventory returns a ManageInventory implementation for VPC activity
func NewManageTenantInventory(config ManageInventoryConfig) ManageTenantInventory {
	return ManageTenantInventory{
		config: config,
	}
}

func tenantFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]string, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	idList, err := grpcServiceClient.FindTenantOrganizationIds(ctx, &corev1.TenantSearchFilter{})
	if err != nil {
		return nil, err
	}
	return idList.GetTenantOrganizationIds(), nil
}

func tenantFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []string) ([]*corev1.Tenant, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	list, err := grpcServiceClient.FindTenantsByOrganizationIds(ctx, &corev1.TenantByOrganizationIdsRequest{
		OrganizationIds: ids,
	})
	if err != nil {
		return nil, err
	}
	return list.GetTenants(), nil
}

func tenantPagedInventory(allItemIDs []string, pagedItems []*corev1.Tenant, input *pagedInventoryInput) *corev1.TenantInventory {
	// Create an inventory page with the subset of Tenants
	inventory := &corev1.TenantInventory{
		Tenants: pagedItems,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if inventory.InventoryPage != nil {
		inventory.InventoryPage.ItemIds = allItemIDs
	}
	return inventory
}
