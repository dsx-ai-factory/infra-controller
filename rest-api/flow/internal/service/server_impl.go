// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package service implements the gRPC server for NICo Flow, the rack-level asset management system.
// It provides APIs for managing rack-level assets including creating, retrieving, and updating
// rack and component information.
//
// TODO: This file is getting large. Consider splitting into multiple module files
package service

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"

	"github.com/google/uuid"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/firmwareauth"
	inventorymanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/manager"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrunmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun/manager"
	taskschedule "github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/taskschedule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	operations "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// FlowServerImpl implements the gRPC Flow server interface.
// It acts as an adapter between gRPC protobuf messages and the internal managers,
// handling protobuf conversion and delegating business logic to the InventoryManager.
type FlowServerImpl struct {
	inventoryManager           inventorymanager.Manager    // Business logic manager for inventory operations
	taskManager                taskmanager.Manager         // Task manager for orchestrating task lifecycle
	taskStore                  taskstore.Store             // Task store for task queries
	taskScheduleStore          taskschedule.Store          // Persistence layer for task schedules
	taskScheduleDispatcher     *taskschedule.Dispatcher    // Background poller that fires due task schedules
	operationRunManager        operationrunmanager.Manager // Operation-run manager for run planning and persistence
	eventRuleManager           eventRuleManager
	conflictResolver           *conflict.Resolver // Reused for inter-schedule conflict detection
	dataCipher                 *secret.Cipher
	pb.UnimplementedFlowServer // Embedded protobuf server interface for forward compatibility
}

// newServerImplementation creates a new Flow gRPC server implementation.
// It initializes the server with the provided managers for handling business logic.
//
// Parameters:
//   - inventoryManager: The inventory manager instance for handling rack and component topology
//   - taskManager: The Task manager for orchestrating task lifecycle
//   - taskStore: The task store for task queries
//   - operationRunManager: The operation-run manager for planning and persisting operation runs
//
// Returns:
//   - *FlowServerImpl: A new server implementation instance
//   - error: Always nil in current implementation, reserved for future error handling
func newServerImplementation(
	inventoryManager inventorymanager.Manager,
	taskManager taskmanager.Manager,
	taskStore taskstore.Store,
	taskScheduleStore taskschedule.Store,
	taskScheduleDispatcher *taskschedule.Dispatcher,
	operationRunManager operationrunmanager.Manager,
	eventRuleManager eventRuleManager,
	dataCipher *secret.Cipher,
) (*FlowServerImpl, error) {
	return &FlowServerImpl{
		inventoryManager:       inventoryManager,
		taskManager:            taskManager,
		taskStore:              taskStore,
		taskScheduleStore:      taskScheduleStore,
		taskScheduleDispatcher: taskScheduleDispatcher,
		operationRunManager:    operationRunManager,
		eventRuleManager:       eventRuleManager,
		dataCipher:             dataCipher,
		conflictResolver:       conflict.NewResolver(taskStore),
	}, nil
}

// Version returns the build information for this Flow service.
// This includes the version, build time, and git commit hash.
func (rs *FlowServerImpl) Version(
	ctx context.Context,
	req *pb.VersionRequest,
) (*pb.BuildInfo, error) {
	return &pb.BuildInfo{
		Version:   metadata.Version,
		BuildTime: metadata.BuildTime,
		GitCommit: metadata.GitCommit,
	}, nil
}

// CreateExpectedRack creates a new expected rack configuration in the system.
// It converts the protobuf rack definition to internal format and stores it
// for later matching against physical rack discoveries.
//
// Parameters:
//   - ctx: Request context for cancellation and deadline management
//   - req: CreateExpectedRackRequest containing the rack configuration
//
// Returns:
//   - *pb.CreateExpectedRackResponse: Response containing the generated or
//     existing or given rack ID
//   - error: Any error that occurred during rack creation
func (rs *FlowServerImpl) CreateExpectedRack(
	ctx context.Context,
	req *pb.CreateExpectedRackRequest,
) (*pb.CreateExpectedRackResponse, error) {
	id, err := rs.inventoryManager.CreateExpectedRack(ctx, protobuf.RackFrom(req.GetRack()))

	return &pb.CreateExpectedRackResponse{Id: protobuf.UUIDTo(id)}, err
}

// GetRackInfoByID retrieves rack information by external rack ID.
// Optionally includes component information if requested.
//
// Parameters:
//   - ctx: Request context for cancellation and deadline management
//   - req: GetRackInfoByIDRequest containing the rack ID and options
//
// Returns:
//   - *pb.GetRackInfoResponse: Response containing the rack information
//   - error: Any error that occurred during rack retrieval
func (rs *FlowServerImpl) GetRackInfoByID(
	ctx context.Context,
	req *pb.GetRackInfoByIDRequest,
) (*pb.GetRackInfoResponse, error) {
	rawID := req.GetId().GetId()
	if rawID == "" {
		return nil, status.Error(codes.InvalidArgument, "rack identifier is required")
	}

	r, err := rs.inventoryManager.GetRackByIdentifier(ctx, externalRackIdentifier(rawID), req.GetWithComponents())
	if err != nil {
		return nil, err
	}

	result := protobuf.RackTo(r)
	if err := rs.populateTaskStats(ctx, []*pb.Rack{result}, nil); err != nil {
		return nil, err
	}
	return &pb.GetRackInfoResponse{Rack: result}, nil
}

func externalRackIdentifier(rawID string) identifier.Identifier {
	return identifier.Identifier{ExternalID: rawID}
}

// GetRackInfoBySerial retrieves rack information by its manufacturer and serial number.
// This allows lookup of racks using their physical identification rather than system-generated ID.
// Optionally includes component information if requested.
//
// Parameters:
//   - ctx: Request context for cancellation and deadline management
//   - req: GetRackInfoBySerialRequest containing manufacturer, serial number, and options
//
// Returns:
//   - *pb.GetRackInfoResponse: Response containing the rack information
//   - error: Any error that occurred during rack retrieval
func (rs *FlowServerImpl) GetRackInfoBySerial(
	ctx context.Context,
	req *pb.GetRackInfoBySerialRequest,
) (*pb.GetRackInfoResponse, error) {
	r, err := rs.inventoryManager.GetRackBySerial(
		ctx,
		req.GetSerialInfo().GetManufacturer(),
		req.GetSerialInfo().GetSerialNumber(),
		req.GetWithComponents(),
	)
	if err != nil {
		return nil, err
	}

	result := protobuf.RackTo(r)
	if err := rs.populateTaskStats(ctx, []*pb.Rack{result}, nil); err != nil {
		return nil, err
	}
	return &pb.GetRackInfoResponse{Rack: result}, nil
}

// PatchRack updates an existing rack configuration with new information.
// This method performs intelligent merging of rack and component data, creating
// new components as needed and updating existing ones. It returns a detailed
// report of all operations performed during the patching process.
//
// Parameters:
//   - ctx: Request context for cancellation and deadline management
//   - req: PatchRackRequest containing the updated rack configuration
//
// Returns:
//   - *pb.PatchRackResponse: Response containing a JSON report of patch operations
//   - error: Any error that occurred during rack patching
func (rs *FlowServerImpl) PatchRack(
	ctx context.Context,
	req *pb.PatchRackRequest,
) (*pb.PatchRackResponse, error) {
	r := protobuf.RackFrom(req.GetRack())

	report, err := rs.inventoryManager.PatchRack(ctx, r)

	return &pb.PatchRackResponse{
		Report: report,
	}, err
}

// AddComponent creates a single component. The component may optionally be
// attached to an existing rack via component.rack_id; if rack_id is not set
// the component is ingested without a rack assignment and can be moved into a
// rack later via PatchComponent.
func (rs *FlowServerImpl) AddComponent(
	ctx context.Context,
	req *pb.AddComponentRequest,
) (*pb.AddComponentResponse, error) {
	pbComp := req.GetComponent()
	if pbComp == nil {
		return nil, errors.New("component is required")
	}

	// Convert proto component to internal; rack_id comes from the component
	// itself and is optional.
	comp := protobuf.ComponentFrom(pbComp)
	comp.RackID = protobuf.UUIDFrom(pbComp.GetRackId())

	// Verify the rack exists only when one has been specified.
	if comp.RackID != uuid.Nil {
		if _, err := rs.inventoryManager.GetRackByID(ctx, comp.RackID, false); err != nil {
			return nil, fmt.Errorf("rack not found: %w", err)
		}
	}

	// Ensure the component has an ID
	if comp.Info.ID == uuid.Nil {
		comp.Info.ID = uuid.New()
	}

	id, err := rs.inventoryManager.AddComponent(ctx, comp)
	if err != nil {
		return nil, fmt.Errorf("failed to add component: %w", err)
	}

	// Re-read the created component to return full state
	created, err := rs.inventoryManager.GetComponentByID(ctx, id)
	if err != nil {
		return nil, fmt.Errorf("failed to get created component: %w", err)
	}

	return &pb.AddComponentResponse{
		Component: protobuf.ComponentTo(created),
	}, nil
}

// DeleteComponent soft-deletes a component by UUID.
func (rs *FlowServerImpl) DeleteComponent(
	ctx context.Context,
	req *pb.DeleteComponentRequest,
) (*pb.DeleteComponentResponse, error) {
	compID := protobuf.UUIDFrom(req.GetId())
	if compID == uuid.Nil {
		return nil, errors.New("component id is required")
	}

	// Verify the component exists
	if _, err := rs.inventoryManager.GetComponentByID(ctx, compID); err != nil {
		return nil, fmt.Errorf("component not found: %w", err)
	}

	if err := rs.inventoryManager.DeleteComponent(ctx, compID); err != nil {
		return nil, fmt.Errorf("failed to delete component: %w", err)
	}

	return &pb.DeleteComponentResponse{}, nil
}

// DeleteRack soft-deletes a rack and all its components.
func (rs *FlowServerImpl) DeleteRack(
	ctx context.Context,
	req *pb.DeleteRackRequest,
) (*pb.DeleteRackResponse, error) {
	rackID := protobuf.UUIDFrom(req.GetId())
	if rackID == uuid.Nil {
		return nil, errors.New("rack id is required")
	}

	if err := rs.inventoryManager.DeleteRack(ctx, rackID); err != nil {
		return nil, fmt.Errorf("failed to delete rack: %w", err)
	}

	return &pb.DeleteRackResponse{}, nil
}

// PurgeRack permanently removes a soft-deleted rack and its components.
func (rs *FlowServerImpl) PurgeRack(
	ctx context.Context,
	req *pb.PurgeRackRequest,
) (*pb.PurgeRackResponse, error) {
	rackID := protobuf.UUIDFrom(req.GetId())
	if rackID == uuid.Nil {
		return nil, errors.New("rack id is required")
	}

	if err := rs.inventoryManager.PurgeRack(ctx, rackID); err != nil {
		return nil, fmt.Errorf("failed to purge rack: %w", err)
	}

	return &pb.PurgeRackResponse{}, nil
}

// PurgeComponent permanently removes a soft-deleted component.
func (rs *FlowServerImpl) PurgeComponent(
	ctx context.Context,
	req *pb.PurgeComponentRequest,
) (*pb.PurgeComponentResponse, error) {
	compID := protobuf.UUIDFrom(req.GetId())
	if compID == uuid.Nil {
		return nil, errors.New("component id is required")
	}

	if err := rs.inventoryManager.PurgeComponent(ctx, compID); err != nil {
		return nil, fmt.Errorf("failed to purge component: %w", err)
	}

	return &pb.PurgeComponentResponse{}, nil
}

// PatchComponent updates a single component's patchable fields.
func (rs *FlowServerImpl) PatchComponent(
	ctx context.Context,
	req *pb.PatchComponentRequest,
) (*pb.PatchComponentResponse, error) {
	compID := protobuf.UUIDFrom(req.GetId())
	if compID == uuid.Nil {
		return nil, errors.New("component id is required")
	}

	// Get the existing component
	existing, err := rs.inventoryManager.GetComponentByID(ctx, compID)
	if err != nil {
		return nil, fmt.Errorf("failed to get component: %w", err)
	}

	// Apply patch fields
	if req.FirmwareVersion != nil {
		existing.FirmwareVersion = *req.FirmwareVersion
	}

	if req.Position != nil {
		existing.Position.SlotID = int(req.Position.SlotId)
		existing.Position.TrayIndex = int(req.Position.TrayIdx)
		existing.Position.HostID = int(req.Position.HostId)
	}

	if req.Description != nil {
		existing.Info.Description = *req.Description
	}

	if req.RackId != nil {
		rackID := protobuf.UUIDFrom(req.RackId)
		if rackID != uuid.Nil {
			if _, err := rs.inventoryManager.GetRackByID(ctx, rackID, false); err != nil {
				return nil, fmt.Errorf("rack not found: %w", err)
			}
			existing.RackID = rackID
		}
	}

	if len(req.GetBmcs()) > 0 {
		existing.BmcsByType = protobuf.BMCsFrom(req.GetBmcs())
	}

	// Persist the update
	if err := rs.inventoryManager.PatchComponent(ctx, existing); err != nil {
		return nil, fmt.Errorf("failed to patch component: %w", err)
	}

	// Re-read the updated component to return current state
	updated, err := rs.inventoryManager.GetComponentByID(ctx, compID)
	if err != nil {
		return nil, fmt.Errorf("failed to get updated component: %w", err)
	}

	return &pb.PatchComponentResponse{
		Component: protobuf.ComponentTo(updated),
	}, nil
}

// GetComponentInfoByID retrieves component information by external component ID
// or BMC MAC address.
// Optionally includes the parent rack information if requested. This method
// performs a two-step lookup: first retrieving the component and its rack ID,
// then fetching rack details if requested.
//
// Parameters:
//   - ctx: Request context for cancellation and deadline management
//   - req: GetComponentInfoByIDRequest containing the component ID and options
//
// Returns:
//   - *pb.GetComponentInfoResponse: Response containing component and optionally rack information
//   - error: Any error that occurred during component or rack retrieval
func (rs *FlowServerImpl) GetComponentInfoByID(
	ctx context.Context,
	req *pb.GetComponentInfoByIDRequest,
) (*pb.GetComponentInfoResponse, error) {
	c, err := rs.resolveComponentIdentifier(ctx, req.GetId().GetId())
	if err != nil {
		return nil, err
	}
	if c.ComponentID == "" {
		return nil, status.Errorf(codes.FailedPrecondition, "component %q has no external ID", req.GetId().GetId())
	}

	var r *rack.Rack

	if req.GetWithRack() && c.RackID != uuid.Nil {
		// Get the rack information; skip the lookup when the component is
		// not yet attached to a rack.
		r, err = rs.inventoryManager.GetRackByID(ctx, c.RackID, false)
		if err != nil {
			return nil, err
		}
	}

	result := protobuf.ComponentTo(c)
	if err := rs.populateTaskStats(ctx, nil, []*pb.Component{result}); err != nil {
		return nil, err
	}
	return &pb.GetComponentInfoResponse{
		Component: result,
		Rack:      protobuf.RackTo(r),
	}, nil
}

// resolveComponentIdentifier resolves an external component ID or BMC MAC
// address. The identifier must select exactly one component.
func (rs *FlowServerImpl) resolveComponentIdentifier(
	ctx context.Context,
	componentIdentifier string,
) (*component.Component, error) {
	return inventoryresolver.ResolveComponentIdentifier(
		ctx,
		rs.inventoryManager,
		componentIdentifier,
		devicetypes.ComponentTypeUnknown,
	)
}

// GetComponentInfoBySerial retrieves component information by its manufacturer and serial number.
// This allows lookup of components using their physical identification rather than system-generated ID.
// Optionally includes the parent rack information if requested. Like GetComponentInfoByID,
// this method performs a two-step lookup when rack information is requested.
//
// Parameters:
//   - ctx: Request context for cancellation and deadline management
//   - req: GetComponentInfoBySerialRequest containing manufacturer, serial number, and options
//
// Returns:
//   - *pb.GetComponentInfoResponse: Response containing component and optionally rack information
//   - error: Any error that occurred during component or rack retrieval
func (rs *FlowServerImpl) GetComponentInfoBySerial(
	ctx context.Context,
	req *pb.GetComponentInfoBySerialRequest,
) (*pb.GetComponentInfoResponse, error) {
	c, err := rs.inventoryManager.GetComponentBySerial(
		ctx,
		req.GetSerialInfo().GetManufacturer(),
		req.GetSerialInfo().GetSerialNumber(),
		req.GetWithRack(),
	)

	if err != nil {
		return nil, err
	}

	var r *rack.Rack

	if req.GetWithRack() && c.RackID != uuid.Nil {
		// Get the rack information; skip the lookup when the component is
		// not yet attached to a rack.
		r, err = rs.inventoryManager.GetRackByID(ctx, c.RackID, false)
		if err != nil {
			return nil, err
		}
	}

	result := protobuf.ComponentTo(c)
	if err := rs.populateTaskStats(ctx, nil, []*pb.Component{result}); err != nil {
		return nil, err
	}
	return &pb.GetComponentInfoResponse{
		Component: result,
		Rack:      protobuf.RackTo(r),
	}, nil
}

func (rs *FlowServerImpl) GetListOfRacks(
	ctx context.Context,
	req *pb.GetListOfRacksRequest,
) (*pb.GetListOfRacksResponse, error) {
	pg := protobuf.PaginationFrom(req.GetPagination())
	if err := pg.Validate(); err != nil {
		return nil, fmt.Errorf("invalid pagination information: %w", err)
	}

	var orderBy *dbquery.OrderBy
	if req.GetOrderBy() != nil {
		orderBy = protobuf.OrderByFrom(req.GetOrderBy())
		if err := orderBy.Validate(); err != nil {
			return nil, fmt.Errorf("invalid order by: %w", err)
		}
	}

	// Extract filters from the filters array
	var infoFilter *dbquery.StringQueryInfo
	var manufacturerFilter *dbquery.StringQueryInfo
	var modelFilter *dbquery.StringQueryInfo

	if len(req.GetFilters()) > 0 {
		for _, filter := range req.GetFilters() {
			if filter == nil {
				continue
			}
			fieldName, queryInfo, err := protobuf.FilterFrom(filter)
			if err != nil {
				return nil, fmt.Errorf("invalid filter: %w", err)
			}
			if queryInfo == nil {
				continue
			}

			switch fieldName {
			case "name":
				infoFilter = queryInfo
			case "manufacturer":
				manufacturerFilter = queryInfo
			case "description->>'model'":
				modelFilter = queryInfo
			default:
				return nil, fmt.Errorf("unsupported filter field: %s", fieldName)
			}
		}
	}

	// If info filter is not provided, use empty filter (matches all)
	if infoFilter == nil {
		infoFilter = &dbquery.StringQueryInfo{Patterns: []string{}, IsWildcard: false, UseOR: false}
	}

	racks, total, err := rs.inventoryManager.GetListOfRacks(
		ctx,
		*infoFilter,
		manufacturerFilter,
		modelFilter,
		pg,
		orderBy,
		req.GetWithComponents(),
	)
	if err != nil {
		return nil, err
	}

	results := make([]*pb.Rack, 0, len(racks))
	for _, r := range racks {
		results = append(results, protobuf.RackTo(r))
	}
	if err := rs.populateTaskStats(ctx, results, nil); err != nil {
		return nil, err
	}

	return &pb.GetListOfRacksResponse{
		Racks: results,
		Total: total,
	}, nil
}

func (rs *FlowServerImpl) CreateNVLDomain(
	ctx context.Context,
	req *pb.CreateNVLDomainRequest,
) (*pb.CreateNVLDomainResponse, error) {
	id, err := rs.inventoryManager.CreateNVLDomain(
		ctx,
		protobuf.NVLDomainFrom(req.GetNvlDomain()),
	)

	return &pb.CreateNVLDomainResponse{Id: protobuf.UUIDTo(id)}, err
}

func (rs *FlowServerImpl) AttachRacksToNVLDomain(
	ctx context.Context,
	req *pb.AttachRacksToNVLDomainRequest,
) (*emptypb.Empty, error) {
	if req.GetNvlDomainIdentifier() == nil {
		return &emptypb.Empty{}, errors.New(
			"nvl domain identifier is required",
		)
	}

	if req.GetRackIdentifiers() == nil {
		// Nothing to do, return as no error.
		return &emptypb.Empty{}, nil
	}

	rackIDs := make([]identifier.Identifier, 0, len(req.GetRackIdentifiers()))
	for _, pbRackID := range req.GetRackIdentifiers() {
		rackIDs = append(rackIDs, *protobuf.IdentifierFrom(pbRackID))
	}

	return &emptypb.Empty{}, rs.inventoryManager.AttachRacksToNVLDomain(
		ctx,
		*protobuf.IdentifierFrom(req.GetNvlDomainIdentifier()),
		rackIDs,
	)
}

func (rs *FlowServerImpl) DetachRacksFromNVLDomain(
	ctx context.Context,
	req *pb.DetachRacksFromNVLDomainRequest,
) (*emptypb.Empty, error) {
	if req.GetRackIdentifiers() == nil {
		// Nothing to do, return as no error.
		return &emptypb.Empty{}, nil
	}

	rackIDs := make([]identifier.Identifier, 0, len(req.GetRackIdentifiers()))
	for _, pbRackID := range req.GetRackIdentifiers() {
		rackIDs = append(rackIDs, *protobuf.IdentifierFrom(pbRackID))
	}

	return &emptypb.Empty{}, rs.inventoryManager.DetachRacksFromNVLDomain(
		ctx,
		rackIDs,
	)
}

func (rs *FlowServerImpl) GetListOfNVLDomains(
	ctx context.Context,
	req *pb.GetListOfNVLDomainsRequest,
) (*pb.GetListOfNVLDomainsResponse, error) {
	pg := protobuf.PaginationFrom(req.GetPagination())
	if err := pg.Validate(); err != nil {
		return nil, fmt.Errorf("invalid pagination information: %w", err)
	}

	if req.GetInfo() == nil {
		return nil, errors.New("info is required")
	}

	nvlDomains, total, err := rs.inventoryManager.GetListOfNVLDomains(
		ctx,
		*protobuf.StringQueryInfoFrom(req.GetInfo()),
		pg,
	)

	results := make([]*pb.NVLDomain, 0, len(nvlDomains))
	for _, n := range nvlDomains {
		results = append(results, protobuf.NVLDomainTo(n))
	}

	return &pb.GetListOfNVLDomainsResponse{
		NvlDomains: results,
		Total:      total,
	}, err
}

func (rs *FlowServerImpl) GetRacksForNVLDomain(
	ctx context.Context,
	req *pb.GetRacksForNVLDomainRequest,
) (*pb.GetRacksForNVLDomainResponse, error) {
	if req.GetNvlDomainIdentifier() == nil {
		return nil, errors.New(
			"nvl domain identifier is required",
		)
	}

	racks, err := rs.inventoryManager.GetRacksForNVLDomain(
		ctx,
		*protobuf.IdentifierFrom(req.GetNvlDomainIdentifier()),
	)

	results := make([]*pb.Rack, 0, len(racks))
	for _, r := range racks {
		results = append(results, protobuf.RackTo(r))
	}

	return &pb.GetRacksForNVLDomainResponse{Racks: results}, err
}

func (rs *FlowServerImpl) PowerOnRack(
	ctx context.Context,
	req *pb.PowerOnRackRequest,
) (*pb.SubmitTaskResponse, error) {
	return rs.handlePowerControlTask(
		ctx,
		req.GetTargetSpec(),
		req.GetDescription(),
		req.GetQueueOptions(),
		req.GetRuleId(),
		&operations.PowerControlTaskInfo{
			Operation:              operations.PowerOperationPowerOn,
			OverrideReadinessCheck: req.GetOverrideReadinessCheck(),
		},
	)
}

func (rs *FlowServerImpl) PowerOffRack(
	ctx context.Context,
	req *pb.PowerOffRackRequest,
) (*pb.SubmitTaskResponse, error) {
	op := operations.PowerOperationPowerOff
	if req.GetForced() {
		op = operations.PowerOperationForcePowerOff
	}
	return rs.handlePowerControlTask(
		ctx,
		req.GetTargetSpec(),
		req.GetDescription(),
		req.GetQueueOptions(),
		req.GetRuleId(),
		&operations.PowerControlTaskInfo{
			Operation:              op,
			Forced:                 req.GetForced(),
			OverrideReadinessCheck: req.GetOverrideReadinessCheck(),
		},
	)
}

func (rs *FlowServerImpl) PowerResetRack(
	ctx context.Context,
	req *pb.PowerResetRackRequest,
) (*pb.SubmitTaskResponse, error) {
	op := operations.PowerOperationRestart
	if req.GetForced() {
		op = operations.PowerOperationForceRestart
	}
	return rs.handlePowerControlTask(
		ctx,
		req.GetTargetSpec(),
		req.GetDescription(),
		req.GetQueueOptions(),
		req.GetRuleId(),
		&operations.PowerControlTaskInfo{
			Operation:              op,
			Forced:                 req.GetForced(),
			OverrideReadinessCheck: req.GetOverrideReadinessCheck(),
		},
	)
}

func (rs *FlowServerImpl) BringUpRack(
	ctx context.Context,
	req *pb.BringUpRackRequest,
) (*pb.SubmitTaskResponse, error) {
	if rs.taskManager == nil {
		return nil, errors.New(
			"task manager is not available",
		)
	}

	targetSpec := req.GetTargetSpec()
	if targetSpec == nil {
		return nil, errors.New(
			"target_spec is required",
		)
	}

	info := &operations.BringUpTaskInfo{
		RuleID:                 protobuf.UUIDStringFrom(req.GetRuleId()),
		OverrideReadinessCheck: req.GetOverrideReadinessCheck(),
	}
	opReq, err := rs.convertTargetSpecToOperationRequest(
		targetSpec, req.GetDescription(), info,
	)
	if err != nil {
		return nil, err
	}

	opReq.RuleID = protobuf.OptionalUUIDFrom(req.GetRuleId())

	taskIDs, err := rs.taskManager.SubmitTask(ctx, opReq)
	if err != nil {
		return nil, err
	}

	if len(taskIDs) == 0 {
		return nil, errors.New(
			"failed to create any tasks",
		)
	}

	return &pb.SubmitTaskResponse{
		TaskIds: protobuf.UUIDsTo(taskIDs),
	}, nil
}

// IngestRack is a convenience API that triggers component ingestion by reusing
// the BringUp workflow with an ingestion-only rule. This registers expected
// components with their respective component manager services without
// performing power or firmware operations.
func (rs *FlowServerImpl) IngestRack(
	ctx context.Context,
	req *pb.IngestRackRequest,
) (*pb.SubmitTaskResponse, error) {
	if rs.taskManager == nil {
		return nil, errors.New("task manager is not available")
	}

	targetSpec := req.GetTargetSpec()
	if targetSpec == nil {
		return nil, errors.New("target_spec is required")
	}

	info := &operations.BringUpTaskInfo{
		RuleID: protobuf.UUIDStringFrom(req.GetRuleId()),
	}

	opReq, err := rs.convertTargetSpecToOperationRequest(
		targetSpec, req.GetDescription(), info,
	)
	if err != nil {
		return nil, err
	}

	// Override the operation code so the rule resolver picks the
	// ingestion-only rule instead of the full bring-up rule.
	opReq.Operation.Code = taskcommon.OpCodeIngest
	opReq.RuleID = protobuf.OptionalUUIDFrom(req.GetRuleId())

	taskIDs, err := rs.taskManager.SubmitTask(ctx, opReq)
	if err != nil {
		return nil, err
	}

	if len(taskIDs) == 0 {
		return nil, errors.New("failed to create any tasks")
	}

	return &pb.SubmitTaskResponse{
		TaskIds: protobuf.UUIDsTo(taskIDs),
	}, nil
}

// DecommissionRack is not yet available: the Core decommission RPCs
// (DecommissionMachine, DecommissionSwitch, DecommissionPowerShelf) are
// pending. Reject requests synchronously so no tasks are created for work
// that cannot start.
//
// TODO: remove this guard once Core RPCs are merged.
func (rs *FlowServerImpl) DecommissionRack(
	_ context.Context,
	_ *pb.DecommissionRackRequest,
) (*pb.SubmitTaskResponse, error) {
	return nil, status.Error(
		codes.Unimplemented,
		"decommission is not yet available: Core decommission RPCs are pending",
	)
}

// decommissionRackImpl is the real implementation, wired up once Core RPCs exist.
//
//nolint:unused
func (rs *FlowServerImpl) decommissionRackImpl(
	ctx context.Context,
	req *pb.DecommissionRackRequest,
) (*pb.SubmitTaskResponse, error) {
	if rs.taskManager == nil {
		return nil, errors.New(
			"task manager is not available",
		)
	}

	targetSpec := req.GetTargetSpec()
	if targetSpec == nil {
		return nil, errors.New(
			"target_spec is required",
		)
	}
	if targetSpec.GetComponents() != nil {
		return nil, status.Error(
			codes.InvalidArgument,
			"decommission requires rack targets; component targets are not supported",
		)
	}

	info := &operations.DecommissionTaskInfo{
		RuleID: protobuf.UUIDStringFrom(req.GetRuleId()),
	}
	opReq, err := rs.convertTargetSpecToOperationRequest(
		targetSpec, req.GetDescription(), info,
	)
	if err != nil {
		return nil, err
	}

	opReq.ConflictStrategy, opReq.QueueTimeout = protobuf.QueueOptionsFrom(req.GetQueueOptions())
	opReq.RuleID = protobuf.OptionalUUIDFrom(req.GetRuleId())

	taskIDs, err := rs.taskManager.SubmitTask(ctx, opReq)
	if err != nil {
		return nil, err
	}

	if len(taskIDs) == 0 {
		return nil, errors.New(
			"failed to create any tasks",
		)
	}

	return &pb.SubmitTaskResponse{
		TaskIds: protobuf.UUIDsTo(taskIDs),
	}, nil
}

func (rs *FlowServerImpl) handlePowerControlTask(
	ctx context.Context,
	targetSpec *pb.OperationTargetSpec,
	description string,
	queueOptions *pb.QueueOptions,
	pbRuleID *pb.UUID,
	info *operations.PowerControlTaskInfo,
) (*pb.SubmitTaskResponse, error) {
	if rs.taskManager == nil {
		return nil, errors.New("task manager is not available")
	}

	if targetSpec == nil {
		return nil, errors.New("target_spec is required")
	}

	info.RuleID = protobuf.UUIDStringFrom(pbRuleID)

	// Convert pb.OperationTargetSpec to internal operation.Request
	req, err := rs.convertTargetSpecToOperationRequest(targetSpec, description, info)
	if err != nil {
		return nil, err
	}

	req.ConflictStrategy, req.QueueTimeout = protobuf.QueueOptionsFrom(queueOptions)
	req.RuleID = protobuf.OptionalUUIDFrom(pbRuleID)

	// Task Manager handles resolve + split by rack + create tasks
	taskIDs, err := rs.taskManager.SubmitTask(ctx, req)
	if err != nil {
		return nil, err
	}

	if len(taskIDs) == 0 {
		return nil, errors.New("failed to create any tasks")
	}

	return &pb.SubmitTaskResponse{TaskIds: protobuf.UUIDsTo(taskIDs)}, nil
}

// convertTargetSpecToOperationRequest converts pb.OperationTargetSpec to internal operation.Request.
// This is a simple translation without DB queries. Task Manager handles resolve + split.
func (rs *FlowServerImpl) convertTargetSpecToOperationRequest(
	targetSpec *pb.OperationTargetSpec,
	description string,
	info operations.Operation,
) (*operation.Request, error) {
	raw, err := info.Marshal()
	if err != nil {
		return nil, err
	}

	ts, err := protobuf.TargetSpecFrom(targetSpec)
	if err != nil {
		return nil, err
	}

	return &operation.Request{
		Operation: operation.Wrapper{
			Type: info.Type(),
			Code: info.CodeString(),
			Info: raw,
		},
		TargetSpec:  ts,
		Description: description,
	}, nil
}

func (rs *FlowServerImpl) ListTasks(
	ctx context.Context,
	req *pb.ListTasksRequest,
) (*pb.ListTasksResponse, error) {
	var rackID uuid.UUID
	if rawID := req.GetRackId().GetId(); rawID != "" {
		resolved, err := rs.inventoryManager.GetRackByIdentifier(ctx, externalRackIdentifier(rawID), false)
		if err != nil {
			return nil, err
		}
		rackID = resolved.Info.ID
	}
	var componentID uuid.UUID
	if rawID := req.GetComponentId().GetId(); rawID != "" {
		resolved, err := rs.resolveComponentIdentifier(ctx, rawID)
		if err != nil {
			return nil, err
		}
		componentID = resolved.Info.ID
	}
	options := &taskcommon.TaskListOptions{
		TaskType:    taskcommon.TaskTypeUnknown,
		RackID:      rackID,
		ComponentID: componentID,
		ActiveOnly:  req.GetActiveOnly(),
	}

	pagination := protobuf.PaginationFrom(req.GetPagination())
	if err := pagination.Validate(); err != nil {
		return nil, fmt.Errorf("invalid pagination information: %w", err)
	}

	tasks, total, err := rs.taskStore.ListTasks(ctx, options, pagination)
	if err != nil {
		return nil, err
	}

	results := make([]*pb.Task, 0, len(tasks))
	for _, t := range tasks {
		results = append(results, protobuf.TaskTo(t))
	}
	if !req.GetWithReport() {
		stripTaskReports(results)
	}

	return &pb.ListTasksResponse{Tasks: results, Total: total}, nil
}

// stripTaskReports clears the report field on each task in place. Used by
// list-style RPCs whose callers did not opt in to receiving execution
// reports, so the multi-KB report blobs are dropped before they cross the
// gRPC wire and get persisted in any caller-side Temporal payload store.
// Single-task RPCs (GetTasksByIDs, CancelTask) intentionally do not call
// this helper.
func stripTaskReports(tasks []*pb.Task) {
	for _, t := range tasks {
		if t != nil {
			t.Report = ""
		}
	}
}

func (rs *FlowServerImpl) GetTasksByIDs(
	ctx context.Context,
	req *pb.GetTasksByIDsRequest,
) (*pb.GetTasksByIDsResponse, error) {
	taskIDs := make([]uuid.UUID, 0, len(req.GetTaskIds()))
	for _, tid := range req.GetTaskIds() {
		taskIDs = append(taskIDs, protobuf.UUIDFrom(tid))
	}

	tasks, err := rs.taskStore.GetTasks(ctx, taskIDs)
	if err != nil {
		return nil, err
	}

	results := make([]*pb.Task, 0, len(tasks))
	for _, t := range tasks {
		results = append(results, protobuf.TaskTo(t))
	}

	return &pb.GetTasksByIDsResponse{Tasks: results}, nil
}

func (rs *FlowServerImpl) CancelTask(
	ctx context.Context,
	req *pb.CancelTaskRequest,
) (*pb.CancelTaskResponse, error) {
	if rs.taskManager == nil {
		return nil, errors.New("task manager is not available")
	}

	taskID, err := uuid.Parse(req.GetTaskId().GetId())
	if err != nil {
		return nil, fmt.Errorf("invalid task ID: %w", err)
	}

	if err := rs.taskManager.CancelTask(ctx, taskID); err != nil {
		return nil, err
	}

	task, err := rs.taskStore.GetTask(ctx, taskID)
	if err != nil {
		return &pb.CancelTaskResponse{}, nil
	}

	return &pb.CancelTaskResponse{Task: protobuf.TaskTo(task)}, nil
}

// ========================================
// Operation Rules API
// ========================================

func (rs *FlowServerImpl) CreateOperationRule(
	ctx context.Context,
	req *pb.CreateOperationRuleRequest,
) (*pb.CreateOperationRuleResponse, error) {
	// Parse rule definition from JSON
	var ruleDef operationrules.RuleDefinition
	if err := json.Unmarshal([]byte(req.GetRuleDefinitionJson()), &ruleDef); err != nil {
		return nil, fmt.Errorf("invalid rule definition JSON: %w", err)
	}

	// Create rule object
	rule := &operationrules.OperationRule{
		ID:             uuid.New(),
		Name:           req.GetName(),
		Description:    req.GetDescription(),
		OperationType:  protobuf.OperationTypeFromProto(req.GetOperationType()),
		OperationCode:  req.GetOperationCode(),
		RuleDefinition: ruleDef,
		IsDefault:      req.GetIsDefault(),
	}

	// Validate rule
	if err := rule.Validate(); err != nil {
		return nil, fmt.Errorf("rule validation failed: %w", err)
	}

	// Store in database
	if err := rs.taskStore.CreateRule(ctx, rule); err != nil {
		return nil, err
	}

	// Return UUID only
	return &pb.CreateOperationRuleResponse{
		Id: protobuf.UUIDTo(rule.ID),
	}, nil
}

func (rs *FlowServerImpl) UpdateOperationRule(
	ctx context.Context,
	req *pb.UpdateOperationRuleRequest,
) (*emptypb.Empty, error) {
	ruleID := protobuf.UUIDFrom(req.GetRuleId())

	if ruleID == uuid.Nil {
		return nil, errors.New("rule ID is required")
	}

	updates := make(map[string]interface{})

	if req.Name != nil {
		updates["name"] = req.GetName()
	}
	if req.Description != nil {
		updates["description"] = req.GetDescription()
	}
	if req.RuleDefinitionJson != nil {
		var ruleDef operationrules.RuleDefinition
		if err := json.Unmarshal([]byte(req.GetRuleDefinitionJson()), &ruleDef); err != nil {
			return nil, fmt.Errorf("invalid rule definition JSON: %w", err)
		}
		// Validate the rule definition
		if err := ruleDef.Validate(); err != nil {
			return nil, fmt.Errorf("rule definition validation failed: %w", err)
		}
		updates["rule_definition"] = ruleDef
	}
	// Note: is_default is NOT updatable via UpdateRule - use SetRuleAsDefault instead

	if err := rs.taskStore.UpdateRule(ctx, ruleID, updates); err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) DeleteOperationRule(
	ctx context.Context,
	req *pb.DeleteOperationRuleRequest,
) (*emptypb.Empty, error) {
	ruleID := protobuf.UUIDFrom(req.GetRuleId())

	if ruleID == uuid.Nil {
		return nil, errors.New("rule ID is required")
	}

	if err := rs.taskStore.DeleteRule(ctx, ruleID); err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) SetRuleAsDefault(
	ctx context.Context,
	req *pb.SetRuleAsDefaultRequest,
) (*emptypb.Empty, error) {
	ruleID := protobuf.UUIDFrom(req.GetRuleId())

	if ruleID == uuid.Nil {
		return nil, errors.New("rule ID is required")
	}

	if err := rs.taskStore.SetRuleAsDefault(ctx, ruleID); err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) GetOperationRule(
	ctx context.Context,
	req *pb.GetOperationRuleRequest,
) (*pb.OperationRule, error) {
	ruleID := protobuf.UUIDFrom(req.GetRuleId())

	if ruleID == uuid.Nil {
		return nil, errors.New("rule ID is required")
	}

	rule, err := rs.taskStore.GetRule(ctx, ruleID)
	if err != nil {
		return nil, err
	}

	return protobuf.OperationRuleTo(rule)
}

func (rs *FlowServerImpl) ListOperationRules(
	ctx context.Context,
	req *pb.ListOperationRulesRequest,
) (*pb.ListOperationRulesResponse, error) {
	options := &taskcommon.OperationRuleListOptions{
		OperationType: taskcommon.TaskTypeUnknown,
	}

	if req.OperationType != nil {
		options.OperationType = protobuf.OperationTypeFromProto(req.GetOperationType())
	}

	options.IsDefault = req.IsDefault

	pagination := &dbquery.Pagination{
		Offset: 0,
		Limit:  100,
	}
	if req.Offset != nil {
		pagination.Offset = int(req.GetOffset())
	}
	if req.Limit != nil {
		pagination.Limit = int(req.GetLimit())
	}

	rules, total, err := rs.taskStore.ListRules(ctx, options, pagination)
	if err != nil {
		return nil, err
	}

	pbRules := make([]*pb.OperationRule, 0, len(rules))
	for _, rule := range rules {
		pbRule, err := protobuf.OperationRuleTo(rule)
		if err != nil {
			return nil, fmt.Errorf("failed to convert rule to protobuf: %w", err)
		}
		pbRules = append(pbRules, pbRule)
	}

	return &pb.ListOperationRulesResponse{
		Rules:      pbRules,
		TotalCount: total,
	}, nil
}

// ========================================
// Rack-Rule Associations API
// ========================================

func (rs *FlowServerImpl) AssociateRuleWithRack(
	ctx context.Context,
	req *pb.AssociateRuleWithRackRequest,
) (*emptypb.Empty, error) {
	rackID := protobuf.UUIDFrom(req.GetRackId())
	ruleID := protobuf.UUIDFrom(req.GetRuleId())

	if rackID == uuid.Nil {
		return nil, errors.New("rack ID is required")
	}

	if ruleID == uuid.Nil {
		return nil, errors.New("rule ID is required")
	}

	// Associate the rule with the rack (store will extract operation type/code from the rule)
	if err := rs.taskStore.AssociateRuleWithRack(ctx, rackID, ruleID); err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) DisassociateRuleFromRack(
	ctx context.Context,
	req *pb.DisassociateRuleFromRackRequest,
) (*emptypb.Empty, error) {
	rackID := protobuf.UUIDFrom(req.GetRackId())
	opType := protobuf.OperationTypeFromProto(req.GetOperationType())
	operation := req.GetOperationCode()

	if rackID == uuid.Nil {
		return nil, errors.New("rack ID is required")
	}

	if operation == "" {
		return nil, errors.New("operation code is required")
	}

	if err := rs.taskStore.DisassociateRuleFromRack(ctx, rackID, opType, operation); err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) GetRackRuleAssociation(
	ctx context.Context,
	req *pb.GetRackRuleAssociationRequest,
) (*pb.GetRackRuleAssociationResponse, error) {
	rackID := protobuf.UUIDFrom(req.GetRackId())
	opType := protobuf.OperationTypeFromProto(req.GetOperationType())
	operation := req.GetOperationCode()

	if rackID == uuid.Nil {
		return nil, errors.New("rack ID is required")
	}

	if operation == "" {
		return nil, errors.New("operation code is required")
	}

	ruleID, err := rs.taskStore.GetRackRuleAssociation(ctx, rackID, opType, operation)
	if err != nil {
		return nil, err
	}

	if ruleID == nil {
		return &pb.GetRackRuleAssociationResponse{
			RuleId: &pb.UUID{Id: uuid.Nil.String()},
		}, nil
	}

	return &pb.GetRackRuleAssociationResponse{
		RuleId: protobuf.UUIDTo(*ruleID),
	}, nil
}

func (rs *FlowServerImpl) ListRackRuleAssociations(
	ctx context.Context,
	req *pb.ListRackRuleAssociationsRequest,
) (*pb.ListRackRuleAssociationsResponse, error) {
	rackID := protobuf.UUIDFrom(req.GetRackId())

	if rackID == uuid.Nil {
		return nil, errors.New("rack ID is required")
	}

	associations, err := rs.taskStore.ListRackRuleAssociations(ctx, rackID)
	if err != nil {
		return nil, err
	}

	pbAssocs := make([]*pb.RackRuleAssociation, 0, len(associations))
	for _, assoc := range associations {
		pbAssocs = append(pbAssocs, protobuf.RackRuleAssociationTo(assoc))
	}

	return &pb.ListRackRuleAssociationsResponse{
		Associations: pbAssocs,
	}, nil
}

// UpgradeFirmware upgrades firmware for components.
// It uses OperationTargetSpec to specify targets and creates a task via the Task framework.
func (rs *FlowServerImpl) UpgradeFirmware(
	ctx context.Context,
	req *pb.UpgradeFirmwareRequest,
) (*pb.SubmitTaskResponse, error) {
	if rs.taskManager == nil {
		return nil, errors.New("task manager is not available")
	}

	targetSpec := req.GetTargetSpec()
	if targetSpec == nil {
		return nil, errors.New("target_spec is required")
	}

	// Build FirmwareControlTaskInfo
	info := &operations.FirmwareControlTaskInfo{
		Operation:              operations.FirmwareOperationUpgrade,
		TargetVersion:          req.GetTargetVersion(),
		RuleID:                 protobuf.UUIDStringFrom(req.GetRuleId()),
		SubTargets:             req.GetSubTargets(),
		OverrideReadinessCheck: req.GetOverrideReadinessCheck(),
	}
	err := rs.encryptFirmwareAuthenticationData(info, req.GetAuthenticationData())
	if err != nil {
		return nil, firmwareAuthenticationStatusError(err)
	}

	// Parse optional time parameters for scheduled upgrade
	if req.GetStartTime() != nil {
		info.StartTime = req.GetStartTime().AsTime().Unix()
	}
	if req.GetEndTime() != nil {
		info.EndTime = req.GetEndTime().AsTime().Unix()
	}

	// Convert pb.OperationTargetSpec to internal operation.Request
	opReq, err := rs.convertTargetSpecToOperationRequest(targetSpec, req.GetDescription(), info)
	if err != nil {
		return nil, err
	}

	opReq.ConflictStrategy, opReq.QueueTimeout = protobuf.QueueOptionsFrom(
		req.GetQueueOptions(),
	)
	opReq.RuleID = protobuf.OptionalUUIDFrom(req.GetRuleId())

	// Task Manager handles resolve + split by rack + create tasks
	taskIDs, err := rs.taskManager.SubmitTask(ctx, opReq)
	if err != nil {
		return nil, err
	}

	if len(taskIDs) == 0 {
		return nil, errors.New("failed to create any tasks")
	}

	return &pb.SubmitTaskResponse{TaskIds: protobuf.UUIDsTo(taskIDs)}, nil
}

func (rs *FlowServerImpl) encryptFirmwareAuthenticationData(
	info *operations.FirmwareControlTaskInfo,
	authenticationData *pb.FirmwareAuthenticationData,
) error {
	encrypted, err := firmwareauth.Encrypt(
		rs.dataCipher,
		authenticationData,
		info.SubTargets,
	)
	if err != nil {
		return err
	}

	info.AuthenticationData = encrypted
	return nil
}

func firmwareAuthenticationStatusError(err error) error {
	if firmwareauth.IsInvalidData(err) {
		return status.Error(codes.InvalidArgument, err.Error())
	}
	if errors.Is(err, firmwareauth.ErrDataCipherNotConfigured) {
		return status.Error(codes.FailedPrecondition, err.Error())
	}

	return status.Error(codes.Internal, err.Error())
}

// GetComponents retrieves components from local database with filtering, pagination, and ordering support.
// If target_spec is provided, it extracts components from the specified racks, NVLink domains, or components first,
// then applies additional filters (name, manufacturer, model, component_types), pagination, and ordering.
// If target_spec is not provided, it queries all components matching the filters.
func (rs *FlowServerImpl) GetComponents(
	ctx context.Context,
	req *pb.GetComponentsRequest,
) (*pb.GetComponentsResponse, error) {
	pg := protobuf.PaginationFrom(req.GetPagination())
	if err := pg.Validate(); err != nil {
		return nil, fmt.Errorf("invalid pagination information: %w", err)
	}

	var orderBy *dbquery.OrderBy
	if req.GetOrderBy() != nil {
		orderBy = protobuf.OrderByFrom(req.GetOrderBy())
		if err := orderBy.Validate(); err != nil {
			return nil, fmt.Errorf("invalid order by: %w", err)
		}
	}

	// Extract filters from the filters array
	var infoFilter *dbquery.StringQueryInfo
	var manufacturerFilter *dbquery.StringQueryInfo
	var modelFilter *dbquery.StringQueryInfo
	var componentTypes []devicetypes.ComponentType

	if len(req.GetFilters()) > 0 {
		for _, filter := range req.GetFilters() {
			if filter == nil {
				continue
			}
			fieldName, queryInfo, err := protobuf.FilterFrom(filter)
			if err != nil {
				return nil, fmt.Errorf("invalid filter: %w", err)
			}
			if queryInfo == nil {
				continue
			}

			switch fieldName {
			case "name":
				infoFilter = queryInfo
			case "manufacturer":
				manufacturerFilter = queryInfo
			case "model":
				modelFilter = queryInfo
			case "type":
				// Convert string patterns to ComponentType enums. Reject unknown
				// type names rather than silently dropping them, which would
				// otherwise turn a typo into "no type filter" and quietly
				// broaden the result set.
				if len(queryInfo.Patterns) > 0 {
					componentTypes = make([]devicetypes.ComponentType, 0, len(queryInfo.Patterns))
					for _, pattern := range queryInfo.Patterns {
						ct := devicetypes.ComponentTypeFromString(pattern)
						if ct == devicetypes.ComponentTypeUnknown {
							return nil, fmt.Errorf("unsupported component type filter: %s", pattern)
						}
						componentTypes = append(componentTypes, ct)
					}
				}
			default:
				return nil, fmt.Errorf("unsupported filter field: %s", fieldName)
			}
		}
	}

	// If info filter is not provided, use empty filter (matches all)
	if infoFilter == nil {
		infoFilter = &dbquery.StringQueryInfo{Patterns: []string{}, IsWildcard: false, UseOR: false}
	}

	var components []*component.Component
	var total int32

	// If target_spec is provided, extract components from it first, then apply filters
	if req.GetTargetSpec() != nil {
		// Extract components from target_spec.
		targetComponents, err := rs.extractComponentsFromTargetSpec(ctx, req.GetTargetSpec())
		if err != nil {
			return nil, fmt.Errorf("failed to extract components from target_spec: %w", err)
		}

		// Apply additional filters to the extracted components
		filteredComponents := rs.applyComponentFilters(
			targetComponents,
			*infoFilter,
			manufacturerFilter,
			modelFilter,
			componentTypes,
		)

		// Apply ordering
		if orderBy != nil {
			if err := rs.sortComponents(filteredComponents, orderBy); err != nil {
				return nil, fmt.Errorf("failed to sort components: %w", err)
			}
		}

		// Apply pagination
		total = int32(len(filteredComponents))
		start := pg.Offset
		end := start + pg.Limit
		if start > len(filteredComponents) {
			components = []*component.Component{}
		} else if end > len(filteredComponents) {
			components = filteredComponents[start:]
		} else {
			components = filteredComponents[start:end]
		}
	} else {
		// No target_spec provided, query all components matching filters directly from database
		var err error
		components, total, err = rs.inventoryManager.GetListOfComponents(
			ctx,
			*infoFilter,
			manufacturerFilter,
			modelFilter,
			componentTypes,
			pg,
			orderBy,
		)
		if err != nil {
			return nil, err
		}
	}

	results := make([]*pb.Component, 0, len(components))
	for _, c := range components {
		results = append(results, protobuf.ComponentTo(c))
	}
	if err := rs.populateTaskStats(ctx, nil, results); err != nil {
		return nil, err
	}

	return &pb.GetComponentsResponse{
		Components: results,
		Total:      total,
	}, nil
}

// ValidateComponents returns pre-computed drifts between expected (local DB) and actual
// (source system) components. Results are computed asynchronously by the inventory loop,
// so this API returns quickly without calling external systems.
//
// If target_spec is provided, only drifts for the specified components are returned.
// If target_spec is not provided, all drifts are returned.
func (rs *FlowServerImpl) ValidateComponents(
	ctx context.Context,
	req *pb.ValidateComponentsRequest,
) (*pb.ValidateComponentsResponse, error) {
	pg := protobuf.PaginationFrom(req.GetPagination())
	if err := pg.Validate(); err != nil {
		return nil, fmt.Errorf("invalid pagination information: %w", err)
	}

	var orderBy *dbquery.OrderBy
	if req.GetOrderBy() != nil {
		orderBy = protobuf.OrderByFrom(req.GetOrderBy())
		if err := orderBy.Validate(); err != nil {
			return nil, fmt.Errorf("invalid order by: %w", err)
		}
	}

	// Extract filters from the filters array
	var infoFilter *dbquery.StringQueryInfo
	var manufacturerFilter *dbquery.StringQueryInfo
	var modelFilter *dbquery.StringQueryInfo
	var componentTypes []devicetypes.ComponentType

	if len(req.GetFilters()) > 0 {
		for _, filter := range req.GetFilters() {
			if filter == nil {
				continue
			}
			fieldName, queryInfo, err := protobuf.FilterFrom(filter)
			if err != nil {
				return nil, fmt.Errorf("invalid filter: %w", err)
			}
			if queryInfo == nil {
				continue
			}

			switch fieldName {
			case "name":
				infoFilter = queryInfo
			case "manufacturer":
				manufacturerFilter = queryInfo
			case "model":
				modelFilter = queryInfo
			case "type":
				// See note on the matching block in GetComponents: reject
				// unknown type names so that filtering by a typo doesn't
				// silently expand the result set.
				if len(queryInfo.Patterns) > 0 {
					componentTypes = make([]devicetypes.ComponentType, 0, len(queryInfo.Patterns))
					for _, pattern := range queryInfo.Patterns {
						ct := devicetypes.ComponentTypeFromString(pattern)
						if ct == devicetypes.ComponentTypeUnknown {
							return nil, fmt.Errorf("unsupported component type filter: %s", pattern)
						}
						componentTypes = append(componentTypes, ct)
					}
				}
			default:
				return nil, fmt.Errorf("unsupported filter field: %s", fieldName)
			}
		}
	}

	// If info filter is not provided, use empty filter (matches all)
	if infoFilter == nil {
		infoFilter = &dbquery.StringQueryInfo{Patterns: []string{}, IsWildcard: false, UseOR: false}
	}

	hasFilters := len(req.GetFilters()) > 0

	targetSpec := req.GetTargetSpec()

	var storeDrifts []inventorymanager.ComponentDrift
	var filteredComponentCount int32
	var err error

	if targetSpec != nil {
		// Extract component UUIDs from target spec
		components, extractErr := rs.extractComponentsFromTargetSpec(ctx, targetSpec)
		if extractErr != nil {
			return nil, fmt.Errorf("failed to extract components from target_spec: %w", extractErr)
		}

		// Apply filters to narrow down components
		if hasFilters {
			components = rs.applyComponentFilters(components, *infoFilter, manufacturerFilter, modelFilter, componentTypes)
		}

		// Apply ordering
		if orderBy != nil {
			if sortErr := rs.sortComponents(components, orderBy); sortErr != nil {
				return nil, fmt.Errorf("failed to sort components: %w", sortErr)
			}
		}

		filteredComponentCount = int32(len(components))

		componentIDs := make([]uuid.UUID, 0, len(components))
		for _, comp := range components {
			componentIDs = append(componentIDs, comp.Info.ID)
		}

		storeDrifts, err = rs.inventoryManager.GetDriftsByComponentIDs(ctx, componentIDs)
	} else {
		storeDrifts, err = rs.inventoryManager.GetAllDrifts(ctx)
	}

	if err != nil {
		return nil, fmt.Errorf("failed to get drifts: %w", err)
	}

	// Convert store drifts to proto response
	var diffs []*pb.ComponentDiff
	var missingCount, unexpectedCount, mismatchCount, matchCount int32

	for _, sd := range storeDrifts {
		var compUUID *pb.UUID
		if sd.ComponentID != nil {
			compUUID = &pb.UUID{Id: sd.ComponentID.String()}
		}
		componentID := ""
		if sd.ExternalID != nil {
			componentID = *sd.ExternalID
		}

		switch sd.DriftType {
		case "missing_in_expected":
			diffs = append(diffs, &pb.ComponentDiff{
				Type:        pb.DiffType_DIFF_TYPE_UNEXPECTED,
				ComponentId: componentID,
			})
			unexpectedCount++
		case "missing_in_actual":
			if sd.ComponentID == nil {
				return nil, status.Error(codes.FailedPrecondition, "missing component has no inventory reference")
			}
			expected, lookupErr := rs.inventoryManager.GetComponentByID(ctx, *sd.ComponentID)
			if lookupErr != nil {
				return nil, lookupErr
			}
			macAddress := componentBMCMAC(expected)
			if macAddress == "" {
				return nil, status.Error(codes.FailedPrecondition, "missing component has no BMC MAC address")
			}
			diffs = append(diffs, &pb.ComponentDiff{
				Type:                pb.DiffType_DIFF_TYPE_MISSING,
				ComponentMacAddress: macAddress,
			})
			missingCount++
		case "mismatch":
			fieldDiffs := make([]*pb.FieldDiff, 0, len(sd.Diffs))
			for _, fd := range sd.Diffs {
				fieldDiffs = append(fieldDiffs, &pb.FieldDiff{
					FieldName:     fd.FieldName,
					ExpectedValue: fd.ExpectedValue,
					ActualValue:   fd.ActualValue,
				})
			}
			diffs = append(diffs, &pb.ComponentDiff{
				Type:        pb.DiffType_DIFF_TYPE_MISMATCH,
				Id:          compUUID,
				ComponentId: componentID,
				FieldDiffs:  fieldDiffs,
			})
			mismatchCount++
		}
	}

	// Calculate match count: if we have targeted components, matches = targeted - mismatches
	if targetSpec != nil {
		matchCount = filteredComponentCount - missingCount - mismatchCount
		if matchCount < 0 {
			matchCount = 0
		}
	}

	// Apply pagination to diffs
	totalDiffs := int32(len(diffs))
	start := pg.Offset
	end := start + pg.Limit
	if start > len(diffs) {
		diffs = []*pb.ComponentDiff{}
	} else if end > len(diffs) {
		diffs = diffs[start:]
	} else {
		diffs = diffs[start:end]
	}

	return &pb.ValidateComponentsResponse{
		Diffs:           diffs,
		TotalDiffs:      totalDiffs,
		MissingCount:    missingCount,
		UnexpectedCount: unexpectedCount,
		MismatchCount:   mismatchCount,
		MatchCount:      matchCount,
	}, nil
}

func componentBMCMAC(comp *component.Component) string {
	if comp == nil {
		return ""
	}
	macAddresses := make([]string, 0)
	for _, bmcType := range devicetypes.BMCTypes() {
		for _, controller := range comp.BmcsByType[bmcType] {
			if macAddress := controller.MAC.String(); macAddress != "" {
				macAddresses = append(macAddresses, macAddress)
			}
		}
	}
	if len(macAddresses) == 0 {
		return ""
	}
	sort.Strings(macAddresses)
	return macAddresses[0]
}

// applyComponentFilters applies filters to a list of components in memory.
// It filters by name, manufacturer, model, and component types.
func (rs *FlowServerImpl) applyComponentFilters(
	components []*component.Component,
	info dbquery.StringQueryInfo,
	manufacturerFilter *dbquery.StringQueryInfo,
	modelFilter *dbquery.StringQueryInfo,
	componentTypes []devicetypes.ComponentType,
) []*component.Component {
	var filtered []*component.Component

	for _, comp := range components {
		// Filter by component type
		if len(componentTypes) > 0 {
			found := false
			for _, ct := range componentTypes {
				if comp.Type == ct {
					found = true
					break
				}
			}
			if !found {
				continue
			}
		}

		// Filter by name
		if !rs.matchesStringQuery(comp.Info.Name, info) {
			continue
		}

		// Filter by manufacturer
		if manufacturerFilter != nil {
			if !rs.matchesStringQuery(comp.Info.Manufacturer, *manufacturerFilter) {
				continue
			}
		}

		// Filter by model
		if modelFilter != nil {
			if !rs.matchesStringQuery(comp.Info.Model, *modelFilter) {
				continue
			}
		}

		filtered = append(filtered, comp)
	}

	return filtered
}

// matchesStringQuery checks if a string matches the StringQueryInfo criteria.
// With UseOR == true the value matches if any pattern matches; with UseOR ==
// false (AND) the value must match every pattern.
func (rs *FlowServerImpl) matchesStringQuery(value string, query dbquery.StringQueryInfo) bool {
	if len(query.Patterns) == 0 {
		return true
	}

	matches := func(pattern string) bool {
		if !query.IsWildcard {
			return value == pattern
		}
		normalizedPattern := pattern
		if len(normalizedPattern) > 0 && normalizedPattern[0] != '%' && normalizedPattern[len(normalizedPattern)-1] != '%' {
			normalizedPattern = "%" + normalizedPattern + "%"
		}
		return rs.matchesWildcard(value, normalizedPattern)
	}

	if query.UseOR {
		for _, pattern := range query.Patterns {
			if matches(pattern) {
				return true
			}
		}
		return false
	}

	for _, pattern := range query.Patterns {
		if !matches(pattern) {
			return false
		}
	}
	return true
}

// matchesWildcard checks if a string matches a wildcard pattern (simple % matching).
func (rs *FlowServerImpl) matchesWildcard(value, pattern string) bool {
	// Simple wildcard matching: convert pattern to regex-like matching
	// For now, use simple contains check for %pattern% or startsWith/endsWith
	if len(pattern) == 0 {
		return true
	}

	// Remove leading/trailing %
	trimmed := pattern
	startMatch := false
	endMatch := false
	if len(trimmed) > 0 && trimmed[0] == '%' {
		startMatch = true
		trimmed = trimmed[1:]
	}
	if len(trimmed) > 0 && trimmed[len(trimmed)-1] == '%' {
		endMatch = true
		trimmed = trimmed[:len(trimmed)-1]
	}

	if len(trimmed) == 0 {
		return true
	}

	if startMatch && endMatch {
		// Contains (case-insensitive)
		return strings.Contains(strings.ToLower(value), strings.ToLower(trimmed))
	} else if startMatch {
		// Ends with
		return len(value) >= len(trimmed) && strings.HasSuffix(value, trimmed)
	} else if endMatch {
		// Starts with
		return len(value) >= len(trimmed) && strings.HasPrefix(value, trimmed)
	} else {
		// Exact match
		return value == trimmed
	}
}

// sortComponents sorts components according to the OrderBy specification.
func (rs *FlowServerImpl) sortComponents(components []*component.Component, orderBy *dbquery.OrderBy) error {
	if orderBy == nil {
		return nil
	}

	// Support sorting by common fields
	switch orderBy.Column {
	case "name":
		sort.Slice(components, func(i, j int) bool {
			if orderBy.Direction == dbquery.OrderAscending {
				return components[i].Info.Name < components[j].Info.Name
			}
			return components[i].Info.Name > components[j].Info.Name
		})
	case "manufacturer":
		sort.Slice(components, func(i, j int) bool {
			if orderBy.Direction == dbquery.OrderAscending {
				return components[i].Info.Manufacturer < components[j].Info.Manufacturer
			}
			return components[i].Info.Manufacturer > components[j].Info.Manufacturer
		})
	case "model":
		sort.Slice(components, func(i, j int) bool {
			if orderBy.Direction == dbquery.OrderAscending {
				return components[i].Info.Model < components[j].Info.Model
			}
			return components[i].Info.Model > components[j].Info.Model
		})
	case "type":
		sort.Slice(components, func(i, j int) bool {
			typeI := devicetypes.ComponentTypeToString(components[i].Type)
			typeJ := devicetypes.ComponentTypeToString(components[j].Type)
			if orderBy.Direction == dbquery.OrderAscending {
				return typeI < typeJ
			}
			return typeI > typeJ
		})
	default:
		return fmt.Errorf("unsupported order by column: %s", orderBy.Column)
	}

	return nil
}

// extractComponentsByTypes extracts components matching any of the specified types from a rack.
// If compTypes is empty, returns all components.
func extractComponentsByTypes(r *rack.Rack, compTypes []devicetypes.ComponentType) []*component.Component {
	var result []*component.Component

	// If no types specified, return all components
	if len(compTypes) == 0 {
		for i := range r.Components {
			result = append(result, &r.Components[i])
		}
		return result
	}

	// Build a set of allowed types for O(1) lookup
	allowedTypes := make(map[devicetypes.ComponentType]bool)
	for _, ct := range compTypes {
		allowedTypes[ct] = true
	}

	for i := range r.Components {
		if allowedTypes[r.Components[i].Type] {
			result = append(result, &r.Components[i])
		}
	}
	return result
}

// extractComponentsFromTargetSpec parses and validates targetSpec via
// protobuf.TargetSpecFrom (the same converter used by the submission path),
// then resolves each rack, NVLink domain, or component target against the inventory.
// Validation errors (malformed UUIDs, empty names, unknown types) are
// surfaced identically to the submission path rather than deferring to an
// inventory-lookup failure.
func (rs *FlowServerImpl) extractComponentsFromTargetSpec(
	ctx context.Context,
	targetSpec *pb.OperationTargetSpec,
) ([]*component.Component, error) {
	spec, err := protobuf.TargetSpecFrom(targetSpec)
	if err != nil {
		return nil, err
	}

	var components []*component.Component

	for _, rt := range spec.Racks {
		resolved, err := rs.resolveRackTarget(ctx, rt)
		if err != nil {
			return nil, err
		}
		components = append(components, resolved...)
	}

	domainRackTargets, err := inventoryresolver.ResolveNVLDomainRackTargets(
		ctx,
		rs.inventoryManager,
		spec.NVLDomains,
	)
	if err != nil {
		return nil, fmt.Errorf("failed to resolve NVLink domain targets: %w", err)
	}
	for _, rackTarget := range domainRackTargets {
		resolved, err := rs.resolveRackTarget(ctx, rackTarget)
		if err != nil {
			return nil, err
		}
		components = append(components, resolved...)
	}

	for _, ct := range spec.Components {
		resolved, err := rs.fetchComponentTarget(ctx, ct)
		if err != nil {
			return nil, err
		}
		components = append(components, resolved...)
	}

	uniqueComponents := make([]*component.Component, 0, len(components))
	seenComponentIDs := make(map[uuid.UUID]struct{}, len(components))
	for _, comp := range components {
		_, exists := seenComponentIDs[comp.Info.ID]
		if exists {
			continue
		}
		seenComponentIDs[comp.Info.ID] = struct{}{}
		uniqueComponents = append(uniqueComponents, comp)
	}

	return uniqueComponents, nil
}

// resolveRackTarget fetches the rack from inventory and returns its components,
// filtered by rt.ComponentTypes (empty = all types).
func (rs *FlowServerImpl) resolveRackTarget(
	ctx context.Context,
	rt operation.RackTarget,
) ([]*component.Component, error) {
	r, err := rs.inventoryManager.GetRackByIdentifier(ctx, rt.Identifier, true)
	if err != nil {
		return nil, err
	}

	return extractComponentsByTypes(r, rt.ComponentTypes), nil
}

// fetchComponentTarget fetches a single component from inventory by its
// internal UUID, external ID, or BMC MAC address, with an optional type filter.
func (rs *FlowServerImpl) fetchComponentTarget(
	ctx context.Context,
	ct operation.ComponentTarget,
) ([]*component.Component, error) {
	if ct.UUID != uuid.Nil {
		comp, err := rs.inventoryManager.GetComponentByID(ctx, ct.UUID)
		if err != nil {
			return nil, fmt.Errorf("failed to get component by uuid %s: %w", ct.UUID, err)
		}
		return []*component.Component{comp}, nil
	}

	resolved, err := inventoryresolver.ResolveComponentIdentifier(
		ctx,
		rs.inventoryManager,
		ct.External.ID,
		ct.External.Type,
	)
	if err != nil {
		return nil, err
	}
	return []*component.Component{resolved}, nil
}
