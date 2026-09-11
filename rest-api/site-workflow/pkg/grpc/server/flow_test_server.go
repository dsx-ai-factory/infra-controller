// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"context"
	"net"
	"time"

	"github.com/gogo/status"
	"github.com/google/uuid"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/reflection"

	emptypb "google.golang.org/protobuf/types/known/emptypb"

	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

var (
	// FlowDefaultPort is the default port that the Flow server listens at
	FlowDefaultPort = ":11080"
)

// FlowServerImpl implements interface FlowServer
type FlowServerImpl struct {
	flowv1.UnimplementedFlowServer
	racks           map[string]*flowv1.Rack
	components      map[string]*flowv1.Component
	nvlDomains      map[string]*flowv1.NVLDomain
	tasks           map[string]*flowv1.Task
	rackToDomainMap map[string]string // Maps rack ID to domain ID
}

var flowLogger = log.With().Str("Component", "Mock Flow gRPC Server").Logger()

// Version implements interface FlowServer
func (r *FlowServerImpl) Version(ctx context.Context, req *flowv1.VersionRequest) (*flowv1.BuildInfo, error) {
	return &flowv1.BuildInfo{
		Version:   "1.0.0",
		BuildTime: time.Now().Format(time.RFC3339),
		GitCommit: "test-commit",
	}, nil
}

// CreateExpectedRack implements interface FlowServer
func (r *FlowServerImpl) CreateExpectedRack(ctx context.Context, req *flowv1.CreateExpectedRackRequest) (*flowv1.CreateExpectedRackResponse, error) {
	if req == nil || req.Rack == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	rackID := uuid.NewString()
	if req.Rack.Info != nil && req.Rack.Info.Id != nil {
		rackID = req.Rack.Info.Id.Id
	}

	rack := &flowv1.Rack{
		Info: &flowv1.DeviceInfo{
			Id: &flowv1.UUID{Id: rackID},
		},
		Location:   req.Rack.Location,
		Components: req.Rack.Components,
	}

	if req.Rack.Info != nil {
		rack.Info.Name = req.Rack.Info.Name
		rack.Info.Manufacturer = req.Rack.Info.Manufacturer
		rack.Info.SerialNumber = req.Rack.Info.SerialNumber
		if req.Rack.Info.Model != nil {
			rack.Info.Model = req.Rack.Info.Model
		}
		if req.Rack.Info.Description != nil {
			rack.Info.Description = req.Rack.Info.Description
		}
	}

	r.racks[rackID] = rack

	// Store components
	for _, comp := range rack.Components {
		if comp.ComponentId != "" {
			r.components[comp.ComponentId] = comp
		}
	}

	return &flowv1.CreateExpectedRackResponse{
		Id: &flowv1.UUID{Id: rackID},
	}, nil
}

// PatchRack implements interface FlowServer
func (r *FlowServerImpl) PatchRack(ctx context.Context, req *flowv1.PatchRackRequest) (*flowv1.PatchRackResponse, error) {
	if req == nil || req.Rack == nil || req.Rack.Info == nil || req.Rack.Info.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	rackID := req.Rack.Info.Id.Id
	rack, ok := r.racks[rackID]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "Rack with ID not found")
	}

	// Update rack fields
	if req.Rack.Info.Name != "" {
		rack.Info.Name = req.Rack.Info.Name
	}
	if req.Rack.Location != nil {
		rack.Location = req.Rack.Location
	}
	if len(req.Rack.Components) > 0 {
		rack.Components = req.Rack.Components
	}

	return &flowv1.PatchRackResponse{
		Report: "Rack patched successfully",
	}, nil
}

// GetRackInfoByID implements interface FlowServer
func (r *FlowServerImpl) GetRackInfoByID(ctx context.Context, req *flowv1.GetRackInfoByIDRequest) (*flowv1.GetRackInfoResponse, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	rack, ok := r.racks[req.Id.Id]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "Rack with ID not found")
	}

	response := &flowv1.GetRackInfoResponse{
		Rack: rack,
	}

	if !req.WithComponents {
		// Return rack without components
		response.Rack = &flowv1.Rack{
			Info:     rack.Info,
			Location: rack.Location,
		}
	}

	return response, nil
}

// GetRackInfoBySerial implements interface FlowServer
func (r *FlowServerImpl) GetRackInfoBySerial(ctx context.Context, req *flowv1.GetRackInfoBySerialRequest) (*flowv1.GetRackInfoResponse, error) {
	if req == nil || req.SerialInfo == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Find rack by serial number
	for _, rack := range r.racks {
		if rack.Info != nil && rack.Info.SerialNumber == req.SerialInfo.SerialNumber {
			response := &flowv1.GetRackInfoResponse{
				Rack: rack,
			}
			if !req.WithComponents {
				response.Rack = &flowv1.Rack{
					Info:     rack.Info,
					Location: rack.Location,
				}
			}
			return response, nil
		}
	}

	return nil, status.Errorf(codes.NotFound, "Rack with serial number not found")
}

// GetComponentInfoByID implements interface FlowServer
func (r *FlowServerImpl) GetComponentInfoByID(ctx context.Context, req *flowv1.GetComponentInfoByIDRequest) (*flowv1.GetComponentInfoResponse, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Find component by UUID
	for _, comp := range r.components {
		if comp.Info != nil && comp.Info.Id != nil && comp.Info.Id.Id == req.Id.Id {
			response := &flowv1.GetComponentInfoResponse{
				Component: comp,
			}
			if req.WithRack {
				// Find the rack containing this component
				for _, rack := range r.racks {
					for _, rackComp := range rack.Components {
						if rackComp.ComponentId == comp.ComponentId {
							response.Rack = rack
							break
						}
					}
					if response.Rack != nil {
						break
					}
				}
			}
			return response, nil
		}
	}

	return nil, status.Errorf(codes.NotFound, "Component with ID not found")
}

// GetComponentInfoBySerial implements interface FlowServer
func (r *FlowServerImpl) GetComponentInfoBySerial(ctx context.Context, req *flowv1.GetComponentInfoBySerialRequest) (*flowv1.GetComponentInfoResponse, error) {
	if req == nil || req.SerialInfo == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Find component by serial number
	for _, comp := range r.components {
		if comp.Info != nil && comp.Info.SerialNumber == req.SerialInfo.SerialNumber {
			response := &flowv1.GetComponentInfoResponse{
				Component: comp,
			}
			if req.WithRack {
				// Find the rack containing this component
				for _, rack := range r.racks {
					for _, rackComp := range rack.Components {
						if rackComp.ComponentId == comp.ComponentId {
							response.Rack = rack
							break
						}
					}
					if response.Rack != nil {
						break
					}
				}
			}
			return response, nil
		}
	}

	return nil, status.Errorf(codes.NotFound, "Component with serial number not found")
}

// GetListOfRacks implements interface FlowServer
func (r *FlowServerImpl) GetListOfRacks(ctx context.Context, req *flowv1.GetListOfRacksRequest) (*flowv1.GetListOfRacksResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	var racks []*flowv1.Rack
	for _, rack := range r.racks {
		if req.WithComponents {
			racks = append(racks, rack)
		} else {
			racks = append(racks, &flowv1.Rack{
				Info:     rack.Info,
				Location: rack.Location,
			})
		}
	}

	return &flowv1.GetListOfRacksResponse{
		Racks: racks,
		Total: int32(len(racks)),
	}, nil
}

// CreateNVLDomain implements interface FlowServer
func (r *FlowServerImpl) CreateNVLDomain(ctx context.Context, req *flowv1.CreateNVLDomainRequest) (*flowv1.CreateNVLDomainResponse, error) {
	if req == nil || req.NvlDomain == nil || req.NvlDomain.Identifier == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	domainID := uuid.NewString()
	if req.NvlDomain.Identifier.Id != nil {
		domainID = req.NvlDomain.Identifier.Id.Id
	}

	domain := &flowv1.NVLDomain{
		Identifier: &flowv1.Identifier{
			Id:   &flowv1.UUID{Id: domainID},
			Name: req.NvlDomain.Identifier.Name,
		},
	}

	r.nvlDomains[domainID] = domain

	return &flowv1.CreateNVLDomainResponse{
		Id: &flowv1.UUID{Id: domainID},
	}, nil
}

// AttachRacksToNVLDomain implements interface FlowServer
func (r *FlowServerImpl) AttachRacksToNVLDomain(ctx context.Context, req *flowv1.AttachRacksToNVLDomainRequest) (*emptypb.Empty, error) {
	if req == nil || req.NvlDomainIdentifier == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	domainID := ""
	if req.NvlDomainIdentifier.Id != nil {
		domainID = req.NvlDomainIdentifier.Id.Id
	} else if req.NvlDomainIdentifier.Name != "" {
		// Find domain by name
		for id, domain := range r.nvlDomains {
			if domain.Identifier != nil && domain.Identifier.Name == req.NvlDomainIdentifier.Name {
				domainID = id
				break
			}
		}
	}

	if domainID == "" {
		return nil, status.Errorf(codes.NotFound, "NVL Domain not found")
	}

	// Attach racks to domain
	for _, rackIdentifier := range req.RackIdentifiers {
		rackID := ""
		if rackIdentifier.Id != nil {
			rackID = rackIdentifier.Id.Id
		} else if rackIdentifier.Name != "" {
			// Find rack by name
			for id, rack := range r.racks {
				if rack.Info != nil && rack.Info.Name == rackIdentifier.Name {
					rackID = id
					break
				}
			}
		}

		if rackID != "" {
			r.rackToDomainMap[rackID] = domainID
		}
	}

	return &emptypb.Empty{}, nil
}

// DetachRacksFromNVLDomain implements interface FlowServer
func (r *FlowServerImpl) DetachRacksFromNVLDomain(ctx context.Context, req *flowv1.DetachRacksFromNVLDomainRequest) (*emptypb.Empty, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Detach racks from domain
	for _, rackIdentifier := range req.RackIdentifiers {
		rackID := ""
		if rackIdentifier.Id != nil {
			rackID = rackIdentifier.Id.Id
		} else if rackIdentifier.Name != "" {
			// Find rack by name
			for id, rack := range r.racks {
				if rack.Info != nil && rack.Info.Name == rackIdentifier.Name {
					rackID = id
					break
				}
			}
		}

		if rackID != "" {
			delete(r.rackToDomainMap, rackID)
		}
	}

	return &emptypb.Empty{}, nil
}

// GetListOfNVLDomains implements interface FlowServer
func (r *FlowServerImpl) GetListOfNVLDomains(ctx context.Context, req *flowv1.GetListOfNVLDomainsRequest) (*flowv1.GetListOfNVLDomainsResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	var domains []*flowv1.NVLDomain
	for _, domain := range r.nvlDomains {
		domains = append(domains, domain)
	}

	return &flowv1.GetListOfNVLDomainsResponse{
		NvlDomains: domains,
		Total:      int32(len(domains)),
	}, nil
}

// GetRacksForNVLDomain implements interface FlowServer
func (r *FlowServerImpl) GetRacksForNVLDomain(ctx context.Context, req *flowv1.GetRacksForNVLDomainRequest) (*flowv1.GetRacksForNVLDomainResponse, error) {
	if req == nil || req.NvlDomainIdentifier == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	domainID := ""
	if req.NvlDomainIdentifier.Id != nil {
		domainID = req.NvlDomainIdentifier.Id.Id
	} else if req.NvlDomainIdentifier.Name != "" {
		// Find domain by name
		for id, domain := range r.nvlDomains {
			if domain.Identifier != nil && domain.Identifier.Name == req.NvlDomainIdentifier.Name {
				domainID = id
				break
			}
		}
	}

	if domainID == "" {
		return nil, status.Errorf(codes.NotFound, "NVL Domain not found")
	}

	// Find all racks attached to this domain
	var racks []*flowv1.Rack
	for rackID, attachedDomainID := range r.rackToDomainMap {
		if attachedDomainID == domainID {
			if rack, ok := r.racks[rackID]; ok {
				racks = append(racks, rack)
			}
		}
	}

	return &flowv1.GetRacksForNVLDomainResponse{
		Racks: racks,
	}, nil
}

// UpgradeFirmware implements interface FlowServer
func (r *FlowServerImpl) UpgradeFirmware(ctx context.Context, req *flowv1.UpgradeFirmwareRequest) (*flowv1.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	taskID := uuid.NewString()
	task := &flowv1.Task{
		Id:           &flowv1.UUID{Id: taskID},
		Operation:    "UpgradeFirmware",
		Status:       flowv1.TaskStatus_TASK_STATUS_PENDING,
		ExecutorType: flowv1.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL,
		Message:      "Firmware upgrade task created",
	}
	r.tasks[taskID] = task

	return &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: taskID}},
	}, nil
}

// GetComponents implements interface FlowServer
func (r *FlowServerImpl) GetComponents(ctx context.Context, req *flowv1.GetComponentsRequest) (*flowv1.GetComponentsResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	var components []*flowv1.Component
	for _, comp := range r.components {
		components = append(components, comp)
	}

	return &flowv1.GetComponentsResponse{
		Components: components,
		Total:      int32(len(components)),
	}, nil
}

// AddComponent implements interface FlowServer
func (r *FlowServerImpl) AddComponent(ctx context.Context, req *flowv1.AddComponentRequest) (*flowv1.AddComponentResponse, error) {
	if req == nil || req.Component == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	if req.Component.RackId == nil || req.Component.RackId.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "component.rack_id must be set")
	}

	componentID := req.Component.ComponentId
	if componentID == "" {
		componentID = uuid.NewString()
	}

	component := &flowv1.Component{
		Type:            req.Component.Type,
		Info:            req.Component.Info,
		FirmwareVersion: req.Component.FirmwareVersion,
		Position:        req.Component.Position,
		Bmcs:            req.Component.Bmcs,
		ComponentId:     componentID,
		RackId:          req.Component.RackId,
		PowerState:      req.Component.PowerState,
	}

	r.components[componentID] = component

	return &flowv1.AddComponentResponse{
		Component: component,
	}, nil
}

// PatchComponent implements interface FlowServer
func (r *FlowServerImpl) PatchComponent(ctx context.Context, req *flowv1.PatchComponentRequest) (*flowv1.PatchComponentResponse, error) {
	if req == nil || req.Id == nil || req.Id.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Find component by UUID
	var comp *flowv1.Component
	for _, c := range r.components {
		if c.Info != nil && c.Info.Id != nil && c.Info.Id.Id == req.Id.Id {
			comp = c
			break
		}
	}

	if comp == nil {
		return nil, status.Errorf(codes.NotFound, "Component with ID not found")
	}

	// Apply patch fields
	if req.FirmwareVersion != nil {
		comp.FirmwareVersion = *req.FirmwareVersion
	}
	if req.Position != nil {
		comp.Position = req.Position
	}
	if req.RackId != nil {
		comp.RackId = req.RackId
	}

	return &flowv1.PatchComponentResponse{
		Component: comp,
	}, nil
}

// DeleteComponent implements interface FlowServer
func (r *FlowServerImpl) DeleteComponent(ctx context.Context, req *flowv1.DeleteComponentRequest) (*flowv1.DeleteComponentResponse, error) {
	if req == nil || req.Id == nil || req.Id.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Find and delete component by UUID
	found := false
	for key, comp := range r.components {
		if comp.Info != nil && comp.Info.Id != nil && comp.Info.Id.Id == req.Id.Id {
			delete(r.components, key)
			found = true
			break
		}
	}

	if !found {
		return nil, status.Errorf(codes.NotFound, "Component with ID not found")
	}

	return &flowv1.DeleteComponentResponse{}, nil
}

// ValidateComponents implements interface FlowServer
func (r *FlowServerImpl) ValidateComponents(ctx context.Context, req *flowv1.ValidateComponentsRequest) (*flowv1.ValidateComponentsResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	// Get components
	componentsResp, err := r.GetComponents(ctx, &flowv1.GetComponentsRequest{
		TargetSpec: req.TargetSpec,
	})
	if err != nil {
		return nil, err
	}

	// For validation, we treat the components as both expected and actual
	// In the new proto, actual is also a Component (ActualComponent was removed)
	actualComponents := make([]*flowv1.Component, 0, len(componentsResp.Components))
	for _, comp := range componentsResp.Components {
		actualComp := &flowv1.Component{
			Type:            comp.Type,
			Info:            comp.Info,
			FirmwareVersion: comp.FirmwareVersion,
			Position:        comp.Position,
			Bmcs:            comp.Bmcs,
			ComponentId:     comp.ComponentId,
			RackId:          comp.RackId,
			PowerState:      "on",
		}
		actualComponents = append(actualComponents, actualComp)
	}

	expectedMap := make(map[string]*flowv1.Component)
	for _, comp := range componentsResp.Components {
		if comp.ComponentId != "" {
			expectedMap[comp.ComponentId] = comp
		}
	}

	actualMap := make(map[string]*flowv1.Component)
	for _, comp := range actualComponents {
		if comp.ComponentId != "" {
			actualMap[comp.ComponentId] = comp
		}
	}

	var diffs []*flowv1.ComponentDiff
	missingCount := 0
	unexpectedCount := 0
	mismatchCount := 0
	matchCount := 0

	// Find components only in expected
	for compID, expectedComp := range expectedMap {
		if _, exists := actualMap[compID]; !exists {
			diffs = append(diffs, &flowv1.ComponentDiff{
				Type:        flowv1.DiffType_DIFF_TYPE_MISSING,
				ComponentId: compID,
				Expected:    expectedComp,
			})
			missingCount++
		}
	}

	// Find components only in actual
	for compID, actualComp := range actualMap {
		if _, exists := expectedMap[compID]; !exists {
			diffs = append(diffs, &flowv1.ComponentDiff{
				Type:        flowv1.DiffType_DIFF_TYPE_UNEXPECTED,
				ComponentId: compID,
				Actual:      actualComp,
			})
			unexpectedCount++
		}
	}

	// Find components in both (check for drift)
	for compID, expectedComp := range expectedMap {
		if actualComp, exists := actualMap[compID]; exists {
			// Simple comparison: check if firmware version differs
			if expectedComp.FirmwareVersion != actualComp.FirmwareVersion {
				var fieldDiffs []*flowv1.FieldDiff
				fieldDiffs = append(fieldDiffs, &flowv1.FieldDiff{
					FieldName:     "firmware_version",
					ExpectedValue: expectedComp.FirmwareVersion,
					ActualValue:   actualComp.FirmwareVersion,
				})
				diffs = append(diffs, &flowv1.ComponentDiff{
					Type:        flowv1.DiffType_DIFF_TYPE_MISMATCH,
					ComponentId: compID,
					FieldDiffs:  fieldDiffs,
				})
				mismatchCount++
			} else {
				matchCount++
			}
		}
	}

	return &flowv1.ValidateComponentsResponse{
		Diffs:           diffs,
		TotalDiffs:      int32(len(diffs)),
		MissingCount:    int32(missingCount),
		UnexpectedCount: int32(unexpectedCount),
		MismatchCount:   int32(mismatchCount),
		MatchCount:      int32(matchCount),
	}, nil
}

// PowerOnRack implements interface FlowServer
func (r *FlowServerImpl) PowerOnRack(ctx context.Context, req *flowv1.PowerOnRackRequest) (*flowv1.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	taskID := uuid.NewString()
	task := &flowv1.Task{
		Id:           &flowv1.UUID{Id: taskID},
		Operation:    "PowerOnRack",
		Status:       flowv1.TaskStatus_TASK_STATUS_PENDING,
		ExecutorType: flowv1.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL,
		Message:      "Power on task created",
	}
	r.tasks[taskID] = task

	return &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: taskID}},
	}, nil
}

// PowerOffRack implements interface FlowServer
func (r *FlowServerImpl) PowerOffRack(ctx context.Context, req *flowv1.PowerOffRackRequest) (*flowv1.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	taskID := uuid.NewString()
	task := &flowv1.Task{
		Id:           &flowv1.UUID{Id: taskID},
		Operation:    "PowerOffRack",
		Status:       flowv1.TaskStatus_TASK_STATUS_PENDING,
		ExecutorType: flowv1.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL,
		Message:      "Power off task created",
	}
	r.tasks[taskID] = task

	return &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: taskID}},
	}, nil
}

// PowerResetRack implements interface FlowServer
func (r *FlowServerImpl) PowerResetRack(ctx context.Context, req *flowv1.PowerResetRackRequest) (*flowv1.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	taskID := uuid.NewString()
	task := &flowv1.Task{
		Id:           &flowv1.UUID{Id: taskID},
		Operation:    "PowerResetRack",
		Status:       flowv1.TaskStatus_TASK_STATUS_PENDING,
		ExecutorType: flowv1.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL,
		Message:      "Power reset task created",
	}
	r.tasks[taskID] = task

	return &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: taskID}},
	}, nil
}

// BringUpRack implements interface FlowServer
func (r *FlowServerImpl) BringUpRack(ctx context.Context, req *flowv1.BringUpRackRequest) (*flowv1.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	taskID := uuid.NewString()
	task := &flowv1.Task{
		Id:           &flowv1.UUID{Id: taskID},
		Operation:    "BringUpRack",
		Status:       flowv1.TaskStatus_TASK_STATUS_PENDING,
		ExecutorType: flowv1.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL,
		Message:      "Bring up task created",
	}
	r.tasks[taskID] = task

	return &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: taskID}},
	}, nil
}

// ListTasks implements interface FlowServer
func (r *FlowServerImpl) ListTasks(ctx context.Context, req *flowv1.ListTasksRequest) (*flowv1.ListTasksResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	var tasks []*flowv1.Task
	for _, task := range r.tasks {
		if req.ActiveOnly && (task.Status == flowv1.TaskStatus_TASK_STATUS_COMPLETED || task.Status == flowv1.TaskStatus_TASK_STATUS_FAILED) {
			continue
		}
		if req.RackId != nil && task.RackId != nil && task.RackId.Id != req.RackId.Id {
			continue
		}
		if req.ComponentId != nil {
			found := false
			for _, cu := range task.GetComponentUuids() {
				if cu != nil && cu.Id == req.ComponentId.Id {
					found = true
					break
				}
			}
			if !found {
				continue
			}
		}
		tasks = append(tasks, task)
	}

	return &flowv1.ListTasksResponse{
		Tasks: tasks,
		Total: int32(len(tasks)),
	}, nil
}

// GetTasksByIDs implements interface FlowServer
func (r *FlowServerImpl) GetTasksByIDs(ctx context.Context, req *flowv1.GetTasksByIDsRequest) (*flowv1.GetTasksByIDsResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	var tasks []*flowv1.Task
	for _, taskID := range req.TaskIds {
		if task, ok := r.tasks[taskID.Id]; ok {
			tasks = append(tasks, task)
		}
	}

	return &flowv1.GetTasksByIDsResponse{
		Tasks: tasks,
	}, nil
}

// FlowTest starts the Flow test gRPC server
func FlowTest(secs int) {
	listener, err := net.Listen("tcp", FlowDefaultPort)
	if err != nil {
		panic(err)
	}

	var serverOpts []grpc.ServerOption
	if cotel.TransportEnabled() {
		// Create server spans and continue traces from otelgrpc-instrumented
		// clients, matching the real core/flow/nvswitch gRPC servers.
		serverOpts = append(serverOpts, grpc.StatsHandler(otelgrpc.NewServerHandler()))
	}
	s := grpc.NewServer(serverOpts...)
	reflection.Register(s)
	flowv1.RegisterFlowServer(s, &FlowServerImpl{
		racks:           make(map[string]*flowv1.Rack),
		components:      make(map[string]*flowv1.Component),
		nvlDomains:      make(map[string]*flowv1.NVLDomain),
		tasks:           make(map[string]*flowv1.Task),
		rackToDomainMap: make(map[string]string),
	})

	if secs != 0 {
		timer := time.AfterFunc(time.Second*time.Duration(secs), func() {
			s.GracefulStop()
			flowLogger.Info().Msgf("Timer started for: %v seconds", secs)
		})
		defer timer.Stop()
	}

	flowLogger.Info().Msg("Started Flow API server")

	err = s.Serve(listener)
	if err != nil {
		flowLogger.Fatal().Err(err).Msg("Failed to start Flow API server")
	}

	flowLogger.Info().Msg("Stopped Flow API server")
}
