// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"bytes"
	"context"
	"slices"
	"sync"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// coreFirmwareDetailsBatchSize follows Core's fixed contract of 1-64 unique targets,
// independently of its configurable inventory FindByIds limit.
const coreFirmwareDetailsBatchSize = 64

// GetRackFirmwareDetails reads membership once and collects Core and Flow facts
// independently. Both readers finish before assembly, including on cancellation.
func (rs *FlowServerImpl) GetRackFirmwareDetails(ctx context.Context, req *pb.GetRackFirmwareDetailsRequest) (*pb.GetRackFirmwareDetailsResponse, error) {
	if req.GetId() == "" {
		return nil, status.Error(codes.InvalidArgument, "rack identifier is required")
	}
	readCtx, cancel := detailsReadContext(ctx)
	defer cancel()
	if readCtx.Err() != nil {
		return nil, status.FromContextError(readCtx.Err()).Err()
	}
	rack, err := rs.inventoryManager.GetRackByIdentifier(readCtx, externalRackIdentifier(req.GetId()), true)
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if readCtx.Err() != nil {
		return nil, status.FromContextError(readCtx.Err()).Err()
	}
	if err != nil {
		code := status.Code(err)
		if code == codes.NotFound {
			return nil, status.Error(codes.NotFound, "rack not found")
		}
		log.Warn().Str("grpc_status_code", code.String()).Msg("Firmware details rack inventory lookup failed")
		if code != codes.Internal {
			code = codes.Unavailable
		}
		return nil, status.Error(code, "rack inventory could not be read")
	}
	if rack == nil {
		return nil, status.Error(codes.Internal, "rack inventory returned no rack")
	}

	// A stable batch order makes the same members eligible before the deadline.
	// Inventory retains ownership of its slice, so sorting must use a copy.
	members := slices.Clone(rack.Components)
	slices.SortFunc(members, func(a, b component.Component) int {
		return bytes.Compare(a.Info.ID[:], b.Info.ID[:])
	})
	var core map[uuid.UUID]firmwareCoreDetails
	var tasks *pb.FirmwareTaskDetailsList
	var reads sync.WaitGroup
	reads.Add(2)
	go func() {
		defer reads.Done()
		core = rs.readRackCoreFirmwareDetails(readCtx, rack.Info.ID, members)
	}()
	go func() {
		defer reads.Done()
		tasks = rs.readRackFirmwareTasks(readCtx, rack.Info.ID, rack.ExternalID)
	}()
	reads.Wait()
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}

	assembled := assembleRackFirmwareDetails(members, core, tasks)
	details := &pb.RackFirmwareDetails{
		RackId: protobuf.UUIDTo(rack.Info.ID), RackExternalId: rack.ExternalID,
		Actual: assembled.actual.toProto(), Desired: assembled.desired.toProto(),
		Operations: &pb.RackFirmwareOperations{Tasks: assembled.tasks},
	}
	for _, member := range assembled.components {
		entry := &pb.RackComponentFirmwareDetails{
			InternalComponentId: protobuf.UUIDTo(member.internalComponentID),
			ComponentType:       protobuf.ComponentTypeTo(member.componentType),
			Actual:              member.actual, Desired: member.desired,
			Operations: &pb.RackComponentFirmwareOperations{
				Actions: member.actions,
				Tasks:   &pb.FirmwareTaskReferenceList{Error: member.tasksError},
			},
		}
		if member.componentID != "" {
			entry.ComponentId = &member.componentID
		}
		for _, id := range member.taskIDs {
			entry.Operations.Tasks.Ids = append(entry.Operations.Tasks.Ids, protobuf.UUIDTo(id))
		}
		details.Components = append(details.Components, entry)
	}
	response := &pb.GetRackFirmwareDetailsResponse{Details: details}
	err = boundRackFirmwareDetailsResponse(response)
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if err != nil {
		return nil, err
	}
	return response, nil
}

// toProto publishes collection availability without synthesizing a rack version.
func (c rackFirmwareCoverage) toProto() *pb.RackFirmwareCoverage {
	return &pb.RackFirmwareCoverage{
		Total: uint64(c.total), Reporting: uint64(c.reporting),
		Unsupported: uint64(c.unsupported), Unavailable: uint64(c.unavailable),
	}
}

// readRackCoreFirmwareDetails deduplicates valid typed targets before batching.
// A bad inventory link or failed batch cannot erase a different batch's evidence.
func (rs *FlowServerImpl) readRackCoreFirmwareDetails(ctx context.Context, rackID uuid.UUID, members []component.Component) map[uuid.UUID]firmwareCoreDetails {
	result := make(map[uuid.UUID]firmwareCoreDetails, len(members))
	for _, kind := range []devicetypes.ComponentType{devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch, devicetypes.ComponentTypePowerShelf} {
		var ids []string
		membersByTarget := make(map[string][]uuid.UUID)
		for _, member := range members {
			if member.Type != kind || member.ComponentID == "" {
				continue // Assembly supplies Unsupported or NotLinked for these members.
			}
			code := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED
			if rs.firmwareReader != nil {
				id, valid := normalizeCoreComponentTarget(kind, member.ComponentID)
				if valid {
					if len(membersByTarget[id]) == 0 {
						ids = append(ids, id)
					}
					membersByTarget[id] = append(membersByTarget[id], member.Info.ID)
					continue
				}
				code = pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED
			}
			actual, desired, actions := unavailableCoreFirmwareDetails(code)
			result[member.Info.ID] = firmwareCoreDetails{actual: actual, desired: desired, actions: actions}
		}
		for batch := range slices.Chunk(ids, coreFirmwareDetailsBatchSize) {
			var response *corev1.GetComponentFirmwareDetailsResponse
			err := ctx.Err()
			if err == nil {
				response, err = rs.firmwareReader.GetComponentFirmwareDetails(ctx, coreFirmwareDetailsRequest(kind, batch))
			}
			if ctx.Err() != nil {
				err = ctx.Err()
			}
			code := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE
			valid := err == nil && len(response.GetEntries()) == len(batch)
			if valid {
				for i, entry := range response.Entries {
					if !sameCoreComponentIdentity(batch[i], entry.GetComponentId()) {
						valid = false
						break
					}
				}
			}
			if err != nil {
				code = firmwareDetailsReadError(err)
			}
			if !valid && ctx.Err() == nil {
				log.Warn().Str("rack_id", rackID.String()).Int("target_count", len(batch)).
					Str("grpc_status_code", status.Code(err).String()).Str("error_category", code.String()).Msg("Firmware details Core batch lookup failed")
			}
			for i, id := range batch {
				actual, desired, actions := unavailableCoreFirmwareDetails(code)
				if valid {
					actual, desired, actions = coreFirmwareDetailsSections(response.Entries[i])
				}
				for _, memberID := range membersByTarget[id] {
					result[memberID] = firmwareCoreDetails{actual: actual, desired: desired, actions: actions}
				}
			}
		}
	}
	return result
}

// readRackFirmwareTasks collects the rack's saved intent once, including targets
// absent from current membership. Waiting-context and identity errors stay on each task.
func (rs *FlowServerImpl) readRackFirmwareTasks(ctx context.Context, rackID uuid.UUID, externalRackID string) *pb.FirmwareTaskDetailsList {
	tasks, err := rs.taskStore.ListFirmwareTasksForRack(ctx, rackID)
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	if err != nil {
		return &pb.FirmwareTaskDetailsList{Error: firmwareDetailsReadError(err).Enum()}
	}
	result := &pb.FirmwareTaskDetailsList{}
	for _, task := range tasks {
		entry, convertErr := firmwareTaskDetailsToProto(task)
		if convertErr != nil {
			return &pb.FirmwareTaskDetailsList{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()}
		}
		result.Entries = append(result.Entries, entry)
	}
	rs.addFirmwareTaskWaitingContext(ctx, rackID, tasks, result.Entries)
	rs.attachFirmwareTaskExternalTargets(ctx, rackID, externalRackID, result.Entries)
	return result
}

// boundRackFirmwareDetailsResponse removes only optional waiting references.
// Primary selections, their external identities and component evidence must fit
// together or fail explicitly.
func boundRackFirmwareDetailsResponse(response *pb.GetRackFirmwareDetailsResponse) error {
	fits, err := detailsResponseFits(response)
	if err != nil || fits {
		return err
	}
	tasks := response.GetDetails().GetOperations().GetTasks()
	for i, task := range tasks.GetEntries() {
		waiting := task.GetWaitingContext()
		if waiting != nil && waiting.Error == nil && (len(waiting.ActiveConflicts) > 0 || len(waiting.EarlierWaiting) > 0) {
			// Assembly shares nested records with its input; bounding owns this copy.
			task = proto.Clone(task).(*pb.FirmwareTaskDetails)
			task.WaitingContext = &pb.FirmwareTaskWaitingContext{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum()}
			tasks.Entries[i] = task
		}
	}
	fits, err = detailsResponseFits(response)
	if err != nil || fits {
		return err
	}
	return status.Error(codes.ResourceExhausted, "rack firmware details exceed the response size limit")
}
