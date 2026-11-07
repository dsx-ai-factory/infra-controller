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

// ManageExpectedMachineInventory is an activity wrapper for Expected Machine inventory collection and publishing
type ManageExpectedMachineInventory struct {
	siteID                uuid.UUID
	coreGrpcAtomicClient  *cclient.CoreGrpcAtomicClient
	temporalPublishClient tClient.Client
	temporalPublishQueue  string
	cloudPageSize         int
}

type linkedExpectedMachineInfo struct {
	expectedMachine       *corev1.ExpectedMachine
	linkedExpectedMachine *corev1.LinkedExpectedMachine
}

// DiscoverExpectedMachineInventory is an activity to collect Expected Machine inventory and publish to Temporal queue
func (memi *ManageExpectedMachineInventory) DiscoverExpectedMachineInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverExpectedMachineInventory").Logger()
	logger.Info().Msg("Starting activity")

	// Define workflow options
	workflowOptions := tClient.StartWorkflowOptions{
		ID:        "update-expectedmachine-inventory-" + memi.siteID.String(),
		TaskQueue: memi.temporalPublishQueue,
	}

	// Get Site Controller gRPC client
	grpcClient := memi.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call GetAllExpectedMachines to get full list of ExpectedMachines on Site
	emList, err := grpcServiceClient.GetAllExpectedMachines(ctx, &emptypb.Empty{})
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve ExpectedMachines using Core gRPC API")

		// Error encountered before we've published anything, report inventory collection error to Cloud
		inventory := &corev1.ExpectedMachineInventory{
			Timestamp: &timestamppb.Timestamp{
				Seconds: time.Now().Unix(),
			},
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			StatusMsg:       err.Error(),
		}

		_, serr := memi.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedMachineInventory", memi.siteID, inventory)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedMachine inventory error to Cloud")
			return serr
		}
		return err
	}

	// Call GetAllExpectedMachinesLinked to get linked Machine IDs
	linkedList, lerr := grpcServiceClient.GetAllExpectedMachinesLinked(ctx, &emptypb.Empty{})
	if lerr != nil {
		logger.Warn().Err(lerr).Msg("Failed to retrieve linked Machine IDs using Core gRPC API")

		// Fatal error - report inventory collection error to Cloud
		inventory := &corev1.ExpectedMachineInventory{
			Timestamp: &timestamppb.Timestamp{
				Seconds: time.Now().Unix(),
			},
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			StatusMsg:       lerr.Error(),
		}

		_, serr := memi.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedMachineInventory", memi.siteID, inventory)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedMachine inventory error to Cloud")
			return serr
		}
		return lerr
	}

	// LinkedExpectedMachine data is missing ExpectedMachine ID so we build an intermediate map using MAC address
	linkedMachinesByKey := make(map[string]*corev1.LinkedExpectedMachine)
	for _, linked := range linkedList.ExpectedMachines {
		linkedMachinesByKey[linked.BmcMacAddress] = linked
	}

	// Build list of ExpectedMachine paired with LinkedExpectedMachine
	linkedExpectedMachinesInfo := []linkedExpectedMachineInfo{}
	allExpectedMachineIDs := []string{}
	for _, em := range emList.ExpectedMachines {
		// Discard records without ID
		if em.Id == nil || em.Id.Value == "" {
			logger.Warn().Str("MAC", em.BmcMacAddress).Str("Serial", em.ChassisSerialNumber).Msg("Discarding ExpectedMachine without ID")
			continue
		}
		allExpectedMachineIDs = append(allExpectedMachineIDs, em.Id.Value)
		// Find matching LinkedMachine record by MAC address if it exists
		linked := linkedMachinesByKey[em.BmcMacAddress]
		linkedExpectedMachinesInfo = append(linkedExpectedMachinesInfo, linkedExpectedMachineInfo{
			expectedMachine:       em,
			linkedExpectedMachine: linked,
		})
	}
	totalCount := len(linkedExpectedMachinesInfo)

	logger.Info().Int("ExpectedMachine Count", totalCount).Msg("Built ExpectedMachine list")

	if totalCount == 0 {
		inventoryPage := getPagedExpectedMachineInventory([]linkedExpectedMachineInfo{}, allExpectedMachineIDs, totalCount, 1, memi.cloudPageSize, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, "No ExpectedMachines reported by Site Controller")

		_, serr := memi.temporalPublishClient.ExecuteWorkflow(context.Background(), workflowOptions, "UpdateExpectedMachineInventory", memi.siteID, inventoryPage)
		if serr != nil {
			logger.Error().Err(serr).Msg("Failed to publish ExpectedMachine inventory to Cloud")
			return serr
		}
		return nil
	}

	// Calculate total pages needed for Cloud
	totalCloudPages := totalCount / memi.cloudPageSize
	if totalCount%memi.cloudPageSize > 0 {
		totalCloudPages++
	}

	// Publish ExpectedMachine inventory to Cloud in separate chunks
	for cloudPage := 1; cloudPage <= totalCloudPages; cloudPage++ {
		startIndex := (cloudPage - 1) * memi.cloudPageSize
		endIndex := startIndex + memi.cloudPageSize
		if endIndex > totalCount {
			endIndex = totalCount
		}

		pagedWorkflowOptions := tClient.StartWorkflowOptions{
			ID:        fmt.Sprintf("%v-%v", workflowOptions.ID, cloudPage),
			TaskQueue: workflowOptions.TaskQueue,
		}

		// Create an inventory page with the subset of ExpectedMachines
		// Slice the list directly for this page
		pagedInfo := linkedExpectedMachinesInfo[startIndex:endIndex]
		inventoryPage := getPagedExpectedMachineInventory(
			pagedInfo,
			allExpectedMachineIDs,
			totalCount,
			cloudPage,
			memi.cloudPageSize,
			corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			"Successfully retrieved ExpectedMachines from Site Controller",
		)

		logger.Info().Msgf("Publishing ExpectedMachine inventory page %d to Cloud", cloudPage)

		_, serr := memi.temporalPublishClient.ExecuteWorkflow(context.Background(), pagedWorkflowOptions, "UpdateExpectedMachineInventory", memi.siteID, inventoryPage)
		if serr != nil {
			logger.Error().Err(serr).Int("Cloud Page", cloudPage).Msg("Failed to publish ExpectedMachine inventory to Cloud")
			return serr
		}
	}

	return nil
}

// getPagedExpectedMachineInventory returns a subset of ExpectedMachineInventory for a given page
func getPagedExpectedMachineInventory(
	pagedInfo []linkedExpectedMachineInfo,
	allExpectedMachineIDs []string,
	totalCount int,
	page int,
	pageSize int,
	status corev1.InventoryStatus,
	statusMessage string,
) *corev1.ExpectedMachineInventory {
	totalPages := totalCount / pageSize
	if totalCount%pageSize > 0 {
		totalPages++
	}

	// Build lists for this page from the sliced info list
	pagedExpectedMachines := make([]*corev1.ExpectedMachine, 0, len(pagedInfo))
	pagedLinkedMachines := make([]*corev1.LinkedExpectedMachine, 0, len(pagedInfo))

	for _, info := range pagedInfo {
		pagedExpectedMachines = append(pagedExpectedMachines, info.expectedMachine)
		// Only add LinkedExpectedMachine if it exists (it may be nil if no match was found)
		if info.linkedExpectedMachine != nil {
			pagedLinkedMachines = append(pagedLinkedMachines, info.linkedExpectedMachine)
		}
	}

	// Create an inventory page with the subset of ExpectedMachines and matching LinkedMachines
	inventoryPage := &corev1.ExpectedMachineInventory{
		ExpectedMachines: pagedExpectedMachines,
		LinkedMachines:   pagedLinkedMachines,
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
			ItemIds:     itemIDsForPage(page, totalPages, allExpectedMachineIDs),
		},
	}

	return inventoryPage
}

// NewManageExpectedMachineInventory returns a ManageInventory implementation for Expected Machine activity
func NewManageExpectedMachineInventory(siteID uuid.UUID, coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient, temporalPublishClient tClient.Client, temporalPublishQueue string, cloudPageSize int) ManageExpectedMachineInventory {
	return ManageExpectedMachineInventory{
		siteID:                siteID,
		coreGrpcAtomicClient:  coreGrpcAtomicClient,
		temporalPublishClient: temporalPublishClient,
		temporalPublishQueue:  temporalPublishQueue,
		cloudPageSize:         cloudPageSize,
	}
}

// ManageExpectedMachine is an activity wrapper for Expected Machine management
type ManageExpectedMachine struct {
	coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient
}

// NewManageExpectedMachine returns a new ManageExpectedMachine client
func NewManageExpectedMachine(coreGrpcAtomicClient *cclient.CoreGrpcAtomicClient) ManageExpectedMachine {
	return ManageExpectedMachine{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// CreateExpectedMachineOnSite creates Expected Machine with NICo
func (mem *ManageExpectedMachine) CreateExpectedMachineOnSite(ctx context.Context, request *corev1.ExpectedMachine) error {
	logger := log.With().Str("Activity", "CreateExpectedMachineOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty create Expected Machine request")
	} else if request.GetId().GetValue() == "" {
		err = errors.New("received create Expected Machine request without required id field")
	} else if request.GetBmcMacAddress() == "" || request.GetChassisSerialNumber() == "" {
		err = errors.New("received create Expected Machine request with missing MAC or serial")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mem.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call Core gRPC endpoint
	start := time.Now()
	_, err = grpcServiceClient.AddExpectedMachine(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Expected Machine using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// UpdateExpectedMachineOnSite updates Expected Machine on NICo
func (mem *ManageExpectedMachine) UpdateExpectedMachineOnSite(ctx context.Context, request *corev1.ExpectedMachine) error {
	logger := log.With().Str("Activity", "UpdateExpectedMachineOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update Expected Machine request")
	} else if request.GetId().GetValue() == "" {
		err = errors.New("received update Expected Machine request without required id field")
	} else if request.GetBmcMacAddress() == "" || request.GetChassisSerialNumber() == "" {
		err = errors.New("received update Expected Machine request with missing MAC or serial")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mem.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateExpectedMachine(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update Expected Machine using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// DeleteExpectedMachineOnSite deletes Expected Machine on NICo
func (mem *ManageExpectedMachine) DeleteExpectedMachineOnSite(ctx context.Context, request *corev1.ExpectedMachineRequest) error {
	logger := log.With().Str("Activity", "DeleteExpectedMachineOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty delete Expected Machine request")
	} else if request.GetId().GetValue() == "" {
		err = errors.New("received delete Expected Machine request without required id field")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mem.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.DeleteExpectedMachine(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to delete Expected Machine using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// CreateExpectedMachinesOnSite creates multiple Expected Machines with NICo using the nico batch endpoint
func (mem *ManageExpectedMachine) CreateExpectedMachinesOnSite(ctx context.Context, request *corev1.BatchExpectedMachineOperationRequest) (*corev1.BatchExpectedMachineOperationResponse, error) {
	logger := log.With().Str("Activity", "CreateExpectedMachinesOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty batch create Expected Machine request")
	} else if request.GetExpectedMachines() == nil || len(request.GetExpectedMachines().GetExpectedMachines()) == 0 {
		err = errors.New("received batch create Expected Machine request with empty list")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Site Controller gRPC batch endpoint
	grpcClient := mem.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call the batch CreateExpectedMachines endpoint
	start := time.Now()
	response, err := grpcServiceClient.CreateExpectedMachines(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to create Expected Machines using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	// Calculate success/failure counts from results for logging
	successes := 0
	failures := 0
	for _, result := range response.GetResults() {
		if result.GetSuccess() {
			successes++
		} else {
			failures++
		}
	}

	logger.Info().
		Int("Total", len(request.GetExpectedMachines().GetExpectedMachines())).
		Int("Succeeded", successes).
		Int("Failed", failures).
		Dur("grpc_duration", duration).
		Msg("Completed activity")

	return response, nil
}

// CreateExpectedMachineOnFlow is retained as a no-op for compatibility with
// workflow histories recorded before direct Flow writes were removed.
// Remove it after the matching `GetVersion` branch is retired and those
// histories can no longer be replayed.
func (*ManageExpectedMachine) CreateExpectedMachineOnFlow(context.Context, *corev1.ExpectedMachine) error {
	return nil
}

// CreateExpectedMachinesOnFlow is retained as a no-op for compatibility with
// workflow histories recorded before direct Flow writes were removed.
// Remove it after the matching `GetVersion` branch is retired and those
// histories can no longer be replayed.
func (*ManageExpectedMachine) CreateExpectedMachinesOnFlow(context.Context, *corev1.BatchExpectedMachineOperationRequest) error {
	return nil
}

// UpdateExpectedMachinesOnSite updates multiple Expected Machines on NICo using the batch endpoint
func (mem *ManageExpectedMachine) UpdateExpectedMachinesOnSite(ctx context.Context, request *corev1.BatchExpectedMachineOperationRequest) (*corev1.BatchExpectedMachineOperationResponse, error) {
	logger := log.With().Str("Activity", "UpdateExpectedMachinesOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty batch update Expected Machine request")
	} else if request.GetExpectedMachines() == nil || len(request.GetExpectedMachines().GetExpectedMachines()) == 0 {
		err = errors.New("received batch update Expected Machine request with empty list")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Site Controller gRPC batch endpoint
	grpcClient := mem.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cclient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Call the batch UpdateExpectedMachines endpoint
	start := time.Now()
	response, err := grpcServiceClient.UpdateExpectedMachines(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update Expected Machines using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	// Calculate success/failure counts from results for logging
	successes := 0
	failures := 0
	for _, result := range response.GetResults() {
		if result.GetSuccess() {
			successes++
		} else {
			failures++
		}
	}

	logger.Info().
		Int("Total", len(request.GetExpectedMachines().GetExpectedMachines())).
		Int("Succeeded", successes).
		Int("Failed", failures).
		Dur("grpc_duration", duration).
		Msg("Completed activity")

	return response, nil
}
