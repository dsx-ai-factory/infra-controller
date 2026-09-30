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

// ManageExpectedRackInventory is an activity wrapper for Expected Rack inventory collection and publishing
type ManageExpectedRackInventory struct {
	siteID                uuid.UUID
	coreGrpcAtomicClient  *cclient.CoreGrpcAtomicClient
	temporalPublishClient tClient.Client
	temporalPublishQueue  string
	cloudPageSize         int
}

// DiscoverExpectedRackInventory is an activity to collect Expected Rack inventory and publish to Temporal queue
func (meri *ManageExpectedRackInventory) DiscoverExpectedRackInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverExpectedRackInventory").Logger()
	logger.Info().Msg("Starting activity")

	// Define workflow options
	workflowOptions := tClient.StartWorkflowOptions{
		ID:        "update-expectedrack-inventory-" + meri.siteID.String(),
		TaskQueue: meri.temporalPublishQueue,
	}

	// Get Site Controller gRPC client
	grpcClient := meri.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call GetAllExpectedRacks to get full list of ExpectedRacks on Site
	erList, err := grpcServiceClient.GetAllExpectedRacks(ctx, &emptypb.Empty{})
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve ExpectedRacks using Core gRPC API")

		// Error encountered before we've published anything, report inventory collection error to Cloud
		inventory := &corev1.ExpectedRackInventory{
			Timestamp: &timestamppb.Timestamp{
				Seconds: time.Now().Unix(),
			},
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			StatusMsg:       err.Error(),
		}

		_, serr := meri.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedRackInventory", meri.siteID, inventory)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedRack inventory error to Cloud")
			return serr
		}
		return err
	}

	// Build list of ExpectedRacks, skipping any record without a rack_id
	expectedRacks := []*corev1.ExpectedRack{}
	allExpectedRackIDs := []string{}
	for _, er := range erList.GetExpectedRacks() {
		// Discard records without rack_id
		if er.GetRackId().GetId() == "" {
			logger.Warn().Msg("Discarding ExpectedRack without rack_id")
			continue
		}
		allExpectedRackIDs = append(allExpectedRackIDs, er.GetRackId().GetId())
		expectedRacks = append(expectedRacks, er)
	}
	totalCount := len(expectedRacks)

	logger.Info().Int("ExpectedRack Count", totalCount).Msg("Built ExpectedRack list")

	if totalCount == 0 {
		inventoryPage := getPagedExpectedRackInventory([]*corev1.ExpectedRack{}, allExpectedRackIDs, totalCount, 1, meri.cloudPageSize, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, "No ExpectedRacks reported by Site Controller")

		_, serr := meri.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedRackInventory", meri.siteID, inventoryPage)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedRack inventory to Cloud")
			return serr
		}
		return nil
	}

	// Calculate total pages needed for Cloud
	totalCloudPages := totalCount / meri.cloudPageSize
	if totalCount%meri.cloudPageSize > 0 {
		totalCloudPages++
	}

	// Publish ExpectedRack inventory to Cloud in separate chunks
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

		// Create an inventory page with the subset of ExpectedRacks
		pagedRacks := expectedRacks[startIndex:endIndex]
		inventoryPage := getPagedExpectedRackInventory(
			pagedRacks,
			allExpectedRackIDs,
			totalCount,
			cloudPage,
			meri.cloudPageSize,
			corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			"Successfully retrieved ExpectedRacks from Site Controller",
		)

		logger.Info().Msgf("Publishing ExpectedRack inventory page %d to Cloud", cloudPage)

		_, serr := meri.temporalPublishClient.ExecuteWorkflow(context.Background(), pagedWorkflowOptions, "UpdateExpectedRackInventory", meri.siteID, inventoryPage)
		if serr != nil {
			logger.Error().Err(serr).Int("Cloud Page", cloudPage).Msg("Failed to publish ExpectedRack inventory to Cloud")
			return serr
		}
	}

	return nil
}

// getPagedExpectedRackInventory returns a subset of ExpectedRackInventory for a given page
func getPagedExpectedRackInventory(
	pagedRacks []*corev1.ExpectedRack,
	allExpectedRackIDs []string,
	totalCount int,
	page int,
	pageSize int,
	status corev1.InventoryStatus,
	statusMessage string,
) *corev1.ExpectedRackInventory {
	totalPages := totalCount / pageSize
	if totalCount%pageSize > 0 {
		totalPages++
	}

	// Create an inventory page with the subset of ExpectedRacks
	inventoryPage := &corev1.ExpectedRackInventory{
		ExpectedRacks: pagedRacks,
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
			ItemIds:     allExpectedRackIDs,
		},
	}

	return inventoryPage
}

// NewManageExpectedRackInventory returns a ManageInventory implementation for Expected Rack activity
func NewManageExpectedRackInventory(siteID uuid.UUID, coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient, temporalPublishClient tClient.Client, temporalPublishQueue string, cloudPageSize int) ManageExpectedRackInventory {
	return ManageExpectedRackInventory{
		siteID:                siteID,
		coreGrpcAtomicClient:  coreGrpcAtomicClient,
		temporalPublishClient: temporalPublishClient,
		temporalPublishQueue:  temporalPublishQueue,
		cloudPageSize:         cloudPageSize,
	}
}

// ManageExpectedRack is an activity wrapper for Expected Rack management
type ManageExpectedRack struct {
	coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient
}

// NewManageExpectedRack returns a new ManageExpectedRack client
func NewManageExpectedRack(coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient) ManageExpectedRack {
	return ManageExpectedRack{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// CreateExpectedRackOnSite creates Expected Rack with NICo
func (mer *ManageExpectedRack) CreateExpectedRackOnSite(ctx context.Context, request *corev1.ExpectedRack) error {
	logger := log.With().Str("Activity", "CreateExpectedRackOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create Expected Rack request")
	} else if request.GetRackId().GetId() == "" {
		err = errors.New("received create Expected Rack request without required rack_id field")
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
	_, err = grpcServiceClient.AddExpectedRack(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Expected Rack using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// UpdateExpectedRackOnSite updates Expected Rack on NICo
func (mer *ManageExpectedRack) UpdateExpectedRackOnSite(ctx context.Context, request *corev1.ExpectedRack) error {
	logger := log.With().Str("Activity", "UpdateExpectedRackOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update Expected Rack request")
	} else if request.GetRackId().GetId() == "" {
		err = errors.New("received update Expected Rack request without required rack_id field")
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
	_, err = grpcServiceClient.UpdateExpectedRack(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update Expected Rack using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// DeleteExpectedRackOnSite deletes Expected Rack on NICo
func (mer *ManageExpectedRack) DeleteExpectedRackOnSite(ctx context.Context, request *corev1.ExpectedRackRequest) error {
	logger := log.With().Str("Activity", "DeleteExpectedRackOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete Expected Rack request")
	} else if request.GetRackId() == "" {
		err = errors.New("received delete Expected Rack request without required rack_id field")
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
	_, err = grpcServiceClient.DeleteExpectedRack(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete Expected Rack using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// ReplaceAllExpectedRacksOnSite replaces all Expected Racks on NICo with the supplied list
func (mer *ManageExpectedRack) ReplaceAllExpectedRacksOnSite(ctx context.Context, request *corev1.ExpectedRackList) error {
	logger := log.With().Str("Activity", "ReplaceAllExpectedRacksOnSite").Logger()

	logger.Info().Msg("Starting activity")

	// Validate request
	if request == nil {
		err := errors.New("received empty replace Expected Rack list request")
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Validate each entry has required ids
	for i, rack := range request.GetExpectedRacks() {
		if rack.GetRackId().GetId() == "" {
			err := errors.New("received replace Expected Rack request with entry missing rack_id field")
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
	_, err := grpcServiceClient.ReplaceAllExpectedRacks(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to replace all Expected Racks using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// DeleteAllExpectedRacksOnSite deletes all Expected Racks on NICo
func (mer *ManageExpectedRack) DeleteAllExpectedRacksOnSite(ctx context.Context) error {
	logger := log.With().Str("Activity", "DeleteAllExpectedRacksOnSite").Logger()

	logger.Info().Msg("Starting activity")

	// Call Core gRPC API endpoint
	grpcClient := mer.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err := grpcServiceClient.DeleteAllExpectedRacks(ctx, &emptypb.Empty{})
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete all Expected Racks using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// CreateExpectedRackOnFlow is retained as a no-op for compatibility with
// workflow histories recorded before direct Flow writes were removed.
// Remove it after the matching `GetVersion` branch is retired and those
// histories can no longer be replayed.
func (*ManageExpectedRack) CreateExpectedRackOnFlow(context.Context, *corev1.ExpectedRack) error {
	return nil
}
