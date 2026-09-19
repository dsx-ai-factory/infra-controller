// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	tClient "go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	cclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// ManageExpectedRackGroupInventory is an activity wrapper for Expected Rack Group inventory collection and publishing
type ManageExpectedRackGroupInventory struct {
	siteID                uuid.UUID
	coreGrpcAtomicClient  *cclient.CoreGrpcAtomicClient
	temporalPublishClient tClient.Client
	temporalPublishQueue  string
	cloudPageSize         int
}

// DiscoverExpectedRackGroupInventory is an activity to collect Expected Rack Group inventory and publish to Temporal queue
func (meri *ManageExpectedRackGroupInventory) DiscoverExpectedRackGroupInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverExpectedRackGroupInventory").Logger()
	logger.Info().Msg("Starting activity")

	// Define workflow options
	workflowOptions := tClient.StartWorkflowOptions{
		ID:        "update-expectedrackgroup-inventory-" + meri.siteID.String(),
		TaskQueue: meri.temporalPublishQueue,
	}

	// Get Site Controller gRPC client
	grpcClient := meri.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call GetAllExpectedRackGroups to get full list of ExpectedRackGroups on Site
	erList, err := grpcServiceClient.GetAllExpectedRackGroups(ctx, &emptypb.Empty{})
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve ExpectedRackGroups using Core gRPC API")

		// Error encountered before we've published anything, report inventory collection error to Cloud
		inventory := &corev1.ExpectedRackGroupInventory{
			Timestamp: &timestamppb.Timestamp{
				Seconds: time.Now().Unix(),
			},
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			StatusMsg:       err.Error(),
		}

		_, serr := meri.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedRackGroupInventory", meri.siteID, inventory)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedRackGroup inventory error to Cloud")
			return serr
		}
		return err
	}

	// Build the ExpectedRackGroup list, skipping records without a rack_group_id.
	expectedRackGroups := []*corev1.ExpectedRackGroup{}
	allExpectedRackGroupIDs := []string{}
	for _, er := range erList.GetExpectedRackGroups() {
		// Discard records without rack_group_id.
		if er.GetRackGroupId().GetId() == "" {
			logger.Warn().Msg("Discarding ExpectedRackGroup without rack_group_id")
			continue
		}
		allExpectedRackGroupIDs = append(allExpectedRackGroupIDs, er.GetRackGroupId().GetId())
		expectedRackGroups = append(expectedRackGroups, er)
	}
	totalCount := len(expectedRackGroups)

	logger.Info().Int("ExpectedRackGroup Count", totalCount).Msg("Built ExpectedRackGroup list")

	if totalCount == 0 {
		inventoryPage := getPagedExpectedRackGroupInventory([]*corev1.ExpectedRackGroup{}, allExpectedRackGroupIDs, totalCount, 1, meri.cloudPageSize, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, "No ExpectedRackGroups reported by Site Controller")

		_, serr := meri.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedRackGroupInventory", meri.siteID, inventoryPage)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedRackGroup inventory to Cloud")
			return serr
		}
		return nil
	}

	// Calculate total pages needed for Cloud
	totalCloudPages := totalCount / meri.cloudPageSize
	if totalCount%meri.cloudPageSize > 0 {
		totalCloudPages++
	}

	// Publish ExpectedRackGroup inventory to Cloud in separate chunks
	for cloudPage := 1; cloudPage <= totalCloudPages; cloudPage++ {
		startIndex := (cloudPage - 1) * meri.cloudPageSize
		endIndex := startIndex + meri.cloudPageSize
		if endIndex > totalCount {
			endIndex = totalCount
		}

		pagedWorkflowOptions := tClient.StartWorkflowOptions{
			ID:        fmt.Sprintf("%v-%v", workflowOptions.ID, cloudPage),
			TaskQueue: workflowOptions.TaskQueue,
		}

		// Create an inventory page with the subset of ExpectedRackGroups
		pagedRacks := expectedRackGroups[startIndex:endIndex]
		inventoryPage := getPagedExpectedRackGroupInventory(
			pagedRacks,
			allExpectedRackGroupIDs,
			totalCount,
			cloudPage,
			meri.cloudPageSize,
			corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			"Successfully retrieved ExpectedRackGroups from Site Controller",
		)

		logger.Info().Msgf("Publishing ExpectedRackGroup inventory page %d to Cloud", cloudPage)

		_, serr := meri.temporalPublishClient.ExecuteWorkflow(context.Background(), pagedWorkflowOptions, "UpdateExpectedRackGroupInventory", meri.siteID, inventoryPage)
		if serr != nil {
			logger.Error().Err(serr).Int("Cloud Page", cloudPage).Msg("Failed to publish ExpectedRackGroup inventory to Cloud")
			return serr
		}
	}

	return nil
}

// getPagedExpectedRackGroupInventory returns a subset of ExpectedRackGroupInventory for a given page
func getPagedExpectedRackGroupInventory(
	pagedRacks []*corev1.ExpectedRackGroup,
	allExpectedRackGroupIDs []string,
	totalCount int,
	page int,
	pageSize int,
	status corev1.InventoryStatus,
	statusMessage string,
) *corev1.ExpectedRackGroupInventory {
	totalPages := totalCount / pageSize
	if totalCount%pageSize > 0 {
		totalPages++
	}

	// Create an inventory page with the subset of ExpectedRackGroups
	inventoryPage := &corev1.ExpectedRackGroupInventory{
		ExpectedRackGroups: pagedRacks,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: status,
		StatusMsg:       statusMessage,
		InventoryPage: &corev1.InventoryPage{
			TotalPages:  int32(totalPages),
			CurrentPage: int32(page),
			PageSize:    int32(pageSize),
			TotalItems:  int32(totalCount),
			ItemIds:     allExpectedRackGroupIDs,
		},
	}

	return inventoryPage
}

// NewManageExpectedRackGroupInventory returns a ManageInventory implementation for Expected Rack Group activity
func NewManageExpectedRackGroupInventory(siteID uuid.UUID, coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient, temporalPublishClient tClient.Client, temporalPublishQueue string, cloudPageSize int) ManageExpectedRackGroupInventory {
	return ManageExpectedRackGroupInventory{
		siteID:                siteID,
		coreGrpcAtomicClient:  coreGrpcAtomicClient,
		temporalPublishClient: temporalPublishClient,
		temporalPublishQueue:  temporalPublishQueue,
		cloudPageSize:         cloudPageSize,
	}
}

// ManageExpectedRackGroup is an activity wrapper for Expected Rack Group management
type ManageExpectedRackGroup struct {
	coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient
}

// NewManageExpectedRackGroup returns a new ManageExpectedRackGroup client
func NewManageExpectedRackGroup(coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient) ManageExpectedRackGroup {
	return ManageExpectedRackGroup{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// CreateExpectedRackGroupOnSite creates Expected Rack Group with NICo
func (mer *ManageExpectedRackGroup) CreateExpectedRackGroupOnSite(ctx context.Context, request *corev1.ExpectedRackGroup) error {
	logger := log.With().Str("Activity", "CreateExpectedRackGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create Expected Rack Group request")
	} else if request.GetRackGroupId().GetId() == "" {
		err = errors.New("received create Expected Rack Group request without required rack_group_id field")
	} else if request.GetTopology() == "" {
		err = errors.New("received create Expected Rack Group request without required topology field")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mer.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.AddExpectedRackGroup(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Expected Rack Group using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// UpdateExpectedRackGroupOnSite updates Expected Rack Group on NICo
func (mer *ManageExpectedRackGroup) UpdateExpectedRackGroupOnSite(ctx context.Context, request *corev1.ExpectedRackGroup) error {
	logger := log.With().Str("Activity", "UpdateExpectedRackGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update Expected Rack Group request")
	} else if request.GetRackGroupId().GetId() == "" {
		err = errors.New("received update Expected Rack Group request without required rack_group_id field")
	} else if request.GetTopology() == "" {
		err = errors.New("received update Expected Rack Group request without required topology field")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mer.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateExpectedRackGroup(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update Expected Rack Group using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// DeleteExpectedRackGroupOnSite deletes Expected Rack Group on NICo
func (mer *ManageExpectedRackGroup) DeleteExpectedRackGroupOnSite(ctx context.Context, request *corev1.ExpectedRackGroupRequest) error {
	logger := log.With().Str("Activity", "DeleteExpectedRackGroupOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete Expected Rack Group request")
	} else if request.GetRackGroupId() == "" {
		err = errors.New("received delete Expected Rack Group request without required rack_group_id field")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mer.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteExpectedRackGroup(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete Expected Rack Group using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// ReplaceAllExpectedRackGroupsOnSite replaces all Expected Rack Groups on NICo with the supplied list
func (mer *ManageExpectedRackGroup) ReplaceAllExpectedRackGroupsOnSite(ctx context.Context, request *corev1.ExpectedRackGroupList) error {
	logger := log.With().Str("Activity", "ReplaceAllExpectedRackGroupsOnSite").Logger()

	logger.Info().Msg("Starting activity")

	// Validate request
	if request == nil {
		err := errors.New("received empty replace Expected Rack Group list request")
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Validate each entry has required ids
	for i, rack := range request.GetExpectedRackGroups() {
		if rack.GetRackGroupId().GetId() == "" {
			err := errors.New("received replace Expected Rack Group request with entry missing rack_group_id field")
			logger.Warn().Int("index", i).Msg(err.Error())
			return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
		}
		if rack.GetTopology() == "" {
			err := errors.New("received replace Expected Rack Group request with entry missing topology field")
			logger.Warn().Int("index", i).Msg(err.Error())
			return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
		}
	}

	// Call Core gRPC API endpoint
	grpcClient := mer.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err := grpcServiceClient.ReplaceAllExpectedRackGroups(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to replace all Expected Rack Groups using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// DeleteAllExpectedRackGroupsOnSite deletes all Expected Rack Groups on NICo
func (mer *ManageExpectedRackGroup) DeleteAllExpectedRackGroupsOnSite(ctx context.Context) error {
	logger := log.With().Str("Activity", "DeleteAllExpectedRackGroupsOnSite").Logger()

	logger.Info().Msg("Starting activity")

	// Call Core gRPC API endpoint
	grpcClient := mer.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err := grpcServiceClient.DeleteAllExpectedRackGroups(ctx, &emptypb.Empty{})
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete all Expected Rack Groups using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}
