// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"slices"
	"time"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/timestamppb"

	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// ManageMachine is an activity wrapper for Machine management tasks that allows injecting DB access
type ManageMachine struct {
	coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
}

// SetMachineMaintenanceOnSite is an activity to set Machine maintenance mode using Core gRPC API
func (mm *ManageMachine) SetMachineMaintenanceOnSite(ctx context.Context, request *corev1.MaintenanceRequest) error {
	logger := log.With().Str("Activity", "SetMachineMaintenanceActivity").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty Machine maintenance request")
	} else if request.HostId == nil || request.HostId.Id == "" {
		err = errors.New("received Machine maintenance request without Machine ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint to set SetMaintenance request
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.SetMaintenance(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to set Maintenance mode for Machine using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return err
}

// UpdateMachineMetadataOnSite is an activity to update Machine metadata using Core gRPC API
func (mm *ManageMachine) UpdateMachineMetadataOnSite(ctx context.Context, request *corev1.MachineMetadataUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateMachineMetadataOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty Machine metadata update request")
	} else if request.MachineId == nil || request.MachineId.Id == "" {
		err = errors.New("received Machine metadata update request without Machine ID")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint to update Machine metadata
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err = grpcServiceClient.UpdateMachineMetadata(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to update Machine metadata using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return err
}

// CreateMachineHealthReportOnSite applies a health report on the Site controller.
func (mm *ManageMachine) CreateMachineHealthReportOnSite(ctx context.Context, request *corev1.InsertMachineHealthReportRequest) error {
	logger := log.With().Str("Activity", "CreateMachineHealthReportOnSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil || request.MachineId == nil || request.MachineId.Id == "" || request.HealthReportEntry == nil || request.HealthReportEntry.Report == nil {
		return temporal.NewNonRetryableApplicationError("invalid InsertMachineHealthReportRequest request", swe.ErrTypeInvalidRequest, errors.New("missing machine id or health report entry"))
	}

	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err := grpcServiceClient.InsertMachineHealthReport(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to insert Machine health report using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// DeleteMachineHealthReportOnSite removes a health report override on the Site controller.
func (mm *ManageMachine) DeleteMachineHealthReportOnSite(ctx context.Context, request *corev1.RemoveMachineHealthReportRequest) error {
	logger := log.With().Str("Activity", "DeleteMachineHealthReportOnSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil || request.MachineId == nil || request.MachineId.Id == "" || request.Source == "" {
		return temporal.NewNonRetryableApplicationError("invalid RemoveMachineHealthReportRequest request", swe.ErrTypeInvalidRequest, errors.New("missing machine id or source"))
	}

	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	start := time.Now()
	_, err := grpcServiceClient.RemoveMachineHealthReport(ctx, request)
	duration := time.Since(start)
	if err != nil {
		logger.Warn().Err(err).Dur("grpc_duration", duration).Msg("Failed to remove Machine health report using Core gRPC API")
		return swe.WrapErr(err)
	}
	logger.Info().Dur("grpc_duration", duration).Msg("Completed activity")

	return nil
}

// GetDpuMachinesByIDs is an activity to retrieve DPU Machines by IDs with network configuration
func (mm *ManageMachine) GetDpuMachinesByIDs(ctx context.Context, dpuMachineIDs []string) (*corev1.DpuMachineList, error) {
	logger := log.With().Str("Activity", "GetDpuMachinesByIDs").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if len(dpuMachineIDs) == 0 {
		err = errors.New("received GetDpuMachinesByIDs request without DPU Machine IDs")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint to get DPU Machines by IDs
	grpcClient := mm.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, cClient.ErrCoreGrpcClientNotConnected
	}
	grpcServiceClient := grpcClient.GrpcServiceClient()

	// Convert string IDs to MachineId objects
	machineIDs := make([]*corev1.MachineId, 0, len(dpuMachineIDs))
	for _, id := range dpuMachineIDs {
		machineIDs = append(machineIDs, &corev1.MachineId{Id: id})
	}

	request := &corev1.MachinesByIdsRequest{
		MachineIds: machineIDs,
	}

	machineList, err := grpcServiceClient.FindMachinesByIds(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve DPU Machines by IDs using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	// For each DPU machine, fetch the network configuration
	dpuMachines := make([]*corev1.DpuMachine, 0, len(machineList.Machines))
	for _, machine := range machineList.Machines {
		if machine.MachineType == corev1.MachineType_DPU {
			networkConfigReq := &corev1.ManagedHostNetworkConfigRequest{
				DpuMachineId: machine.Id,
			}
			networkConfig, nerr := grpcServiceClient.GetManagedHostNetworkConfig(ctx, networkConfigReq)
			if nerr != nil {
				logger.Warn().Err(nerr).Str("DPU Machine ID", machine.Id.Id).Msg("Failed to retrieve network config for DPU machine, continuing without it")
				// Don't fail the entire request if network config is unavailable
			}

			logger.Debug().Str("DPU Machine ID", machine.Id.Id).Msg("Retrieved network config for DPU machine")
			dpuMachines = append(dpuMachines, &corev1.DpuMachine{
				Machine:          machine,
				DpuNetworkConfig: networkConfig,
			})
		}
	}

	logger.Info().Int("DPU Machine Count", len(dpuMachines)).Msg("Completed activity")

	return &corev1.DpuMachineList{Machines: dpuMachines}, nil
}

// NewManageMachine returns a new ManageMachine activity
func NewManageMachine(coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient) ManageMachine {
	return ManageMachine{
		coreGrpcAtomicClient: coreGrpcAtomicClient,
	}
}

// ManageMachineInventory is an activity wrapper for Machine inventory collection and publishing
type ManageMachineInventory struct {
	config ManageInventoryConfig
}

// CollectAndPublishMachineInventory is an activity to collect Machine inventory and publish to Temporal queue
func (mmi *ManageMachineInventory) CollectAndPublishMachineInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "CollectAndPublishMachineInventory").Logger()
	logger.Info().Msg("Starting activity")
	inventoryImpl := manageInventoryImpl[*corev1.MachineId, *corev1.Machine, *corev1.MachineInventory]{
		itemType:               "Machine",
		config:                 mmi.config,
		internalFindIDs:        machineFindIDs,
		internalFindByIDs:      machineFindByIDs,
		internalPagedInventory: machinePagedInventory,
	}
	return inventoryImpl.CollectAndPublishInventory(ctx, &logger)
}

// NewManageMachineInventory returns a ManageInventory implementation for Machine activity
func NewManageMachineInventory(config ManageInventoryConfig) ManageMachineInventory {
	return ManageMachineInventory{
		config: config,
	}
}

func machineFindIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient) ([]*corev1.MachineId, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	machineIDList, err := grpcServiceClient.FindMachineIds(ctx, &corev1.MachineSearchConfig{})
	if err != nil {
		return nil, err
	}
	return machineIDList.GetMachineIds(), nil
}

func machineFindByIDs(ctx context.Context, grpcClient *cClient.CoreGrpcClient, ids []*corev1.MachineId) ([]*corev1.Machine, error) {
	grpcServiceClient := grpcClient.GrpcServiceClient()
	machineList, err := grpcServiceClient.FindMachinesByIds(ctx, &corev1.MachinesByIdsRequest{
		MachineIds: ids,
	})
	if err != nil {
		return nil, err
	}

	machines := machineList.GetMachines()
	for _, machine := range machines {
		pruneMachineForPublish(machine)
	}

	return machines, nil
}

// maxPublishedMachineEvents bounds the event history a published Machine carries. The REST layer
// reads the events only to date the current state, but a Machine arrives with its whole history,
// which is the largest thing in the message. Twenty keeps recent history worth reading without
// carrying hundreds of entries per Machine.
const maxPublishedMachineEvents = 20

// pruneMachineForPublish drops what the REST layer does not read from a Machine before it is
// published.
//
// Core fills both the fields under status and config and their deprecated twins on the Machine
// itself, which the proto marks for removal once rest-api reads the new ones. The REST layer
// already reads status and config, so the twins are an exact duplicate of about a quarter of every
// Machine, and it persists the whole message as jsonb, so they cost storage as well as Temporal
// payload.
//
// Events are the larger cost. A Machine arrives with its full state history, and the REST layer
// uses it to date one lifecycle transition, so only the events around the current state_version
// are worth sending.
//
//nolint:staticcheck // Clearing the deprecated fields is the point, so SA1019 has nothing to warn about here.
func pruneMachineForPublish(machine *corev1.Machine) {
	if machine == nil {
		return
	}

	machine.Events = recentMachineEvents(machine.GetEvents(), machine.GetStateVersion())

	// Superseded by status.
	machine.Interfaces = nil
	machine.DiscoveryInfo = nil
	machine.LastRebootTime = nil
	machine.LastObservationTime = nil
	machine.AssociatedHostMachineId = nil
	machine.LastRebootRequestedTime = nil
	machine.LastRebootRequestedMode = nil
	machine.DpuAgentVersion = nil
	machine.AssociatedDpuMachineIds = nil
	machine.Health = nil
	machine.HealthSources = nil
	machine.FailureDetails = nil
	machine.IbStatus = nil
	machine.InstanceNetworkRestrictions = nil
	machine.Capabilities = nil
	machine.HwSkuStatus = nil
	machine.QuarantineState = nil
	machine.HwSkuDeviceType = nil
	machine.UpdateComplete = false
	machine.NvlinkInfo = nil
	machine.NvlinkStatusObservation = nil
	machine.SpxStatusObservation = nil
	// LastScoutObservedVersion stays. NewAPIMachine still falls back to it when status does not
	// carry one, and at 26 bytes clearing it would trade a response field for nothing.

	// Superseded by config.
	machine.MaintenanceReference = nil
	machine.MaintenanceStartTime = nil
	machine.FirmwareAutoupdate = nil
	machine.InstanceTypeId = nil
	machine.HwSku = nil
	machine.Dpf = nil
}

// recentMachineEvents keeps the tail of a Machine's event history. Core reports events oldest
// first, so the tail is the newest, and the event recording the current state version is normally
// the last one. The REST layer needs that event to date the current state, so it is carried
// explicitly when it falls outside the tail rather than relying on the reported order.
func recentMachineEvents(events []*corev1.MachineEvent, stateVersion string) []*corev1.MachineEvent {
	if len(events) <= maxPublishedMachineEvents {
		return events
	}

	recent := events[len(events)-maxPublishedMachineEvents:]
	if stateVersion == "" || slices.ContainsFunc(recent, func(event *corev1.MachineEvent) bool {
		return event.GetVersion() == stateVersion
	}) {
		return recent
	}

	for _, event := range events[:len(events)-maxPublishedMachineEvents] {
		if event.GetVersion() == stateVersion {
			return append([]*corev1.MachineEvent{event}, recent...)
		}
	}

	return recent
}

func machinePagedInventory(allItemIDs []*corev1.MachineId, pagedItems []*corev1.Machine, input *pagedInventoryInput) *corev1.MachineInventory {
	itemIDs := []string{}
	for _, id := range allItemIDs {
		itemIDs = append(itemIDs, id.GetId())
	}

	pagedMachineInfo := []*corev1.MachineInfo{}
	for _, machine := range pagedItems {
		pagedMachineInfo = append(pagedMachineInfo, &corev1.MachineInfo{
			Machine: machine,
		})
	}

	machineInventory := &corev1.MachineInventory{
		Machines: pagedMachineInfo,
		Timestamp: &timestamppb.Timestamp{
			Seconds: time.Now().Unix(),
		},
		InventoryStatus: input.status,
		StatusMsg:       input.statusMessage,
		InventoryPage:   input.buildPage(),
	}
	if machineInventory.InventoryPage != nil {
		machineInventory.InventoryPage.ItemIds = itemIDs
	}

	return machineInventory
}
