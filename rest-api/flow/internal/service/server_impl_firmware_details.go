// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"slices"
	"sync"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// GetTrayFirmwareDetails resolves one inventory member, then independently reads
// Core firmware evidence and stored Flow intent. Child calls write separate
// fields and are joined before serialization, including after cancellation.
func (rs *FlowServerImpl) GetTrayFirmwareDetails(ctx context.Context, req *pb.GetTrayFirmwareDetailsRequest) (*pb.GetTrayFirmwareDetailsResponse, error) {
	readCtx, cancel := detailsReadContext(ctx)
	defer cancel()
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if readCtx.Err() != nil {
		return nil, status.FromContextError(readCtx.Err()).Err()
	}
	tray, err := rs.resolveComponentIdentifier(readCtx, req.GetId())
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if readCtx.Err() != nil {
		return nil, status.FromContextError(readCtx.Err()).Err()
	}
	if err != nil {
		switch status.Code(err) {
		case codes.InvalidArgument, codes.NotFound, codes.FailedPrecondition:
			return nil, err
		default:
			// Dependency text and the untrusted selector can contain private data.
			log.Warn().Str("grpc_status_code", status.Code(err).String()).Msg("Firmware details inventory lookup failed")
			code := codes.Unavailable
			if status.Code(err) == codes.Internal {
				code = codes.Internal
			}
			return nil, status.Error(code, "tray inventory could not be read")
		}
	}
	if !supportsComponentDetails(tray.Type) {
		return nil, status.Error(codes.InvalidArgument, "component type does not support firmware details")
	}
	details := &pb.TrayFirmwareDetails{ComponentType: protobuf.ComponentTypeTo(tray.Type), Operations: &pb.FirmwareOperations{}}
	if tray.ComponentID != "" {
		details.ComponentId = &tray.ComponentID
	}
	if tray.RackID != uuid.Nil {
		details.RackId = protobuf.UUIDTo(tray.RackID)
	}
	var reads sync.WaitGroup
	reads.Add(2)
	go func() {
		defer reads.Done()
		details.Actual, details.Desired, details.Operations.Actions = rs.readCoreFirmwareDetails(readCtx, tray)
	}()
	go func() {
		defer reads.Done()
		details.Operations.Tasks = rs.readFlowFirmwareTasks(readCtx, tray)
		rs.addFirmwareTaskExternalTargets(readCtx, tray, details)
	}()
	reads.Wait()
	response := &pb.GetTrayFirmwareDetailsResponse{Details: details}
	err = boundFirmwareDetailsResponse(response)
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if err != nil {
		return nil, err
	}
	return response, nil
}

// readCoreFirmwareDetails verifies the requested identity before accepting any
// section. Absent required sections/results mean unsupported evidence, never an
// empty inventory. Core's nested typed errors remain independent values.
func (rs *FlowServerImpl) readCoreFirmwareDetails(ctx context.Context, tray *component.Component) (*pb.FirmwareActualDetails, *pb.FirmwareDesiredDetails, *pb.FirmwareActionDetails) {
	if tray.ComponentID == "" {
		return unavailableCoreFirmwareDetails(pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED)
	}
	if rs.firmwareReader == nil {
		return unavailableCoreFirmwareDetails(pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED)
	}
	request := &corev1.GetComponentFirmwareDetailsRequest{}
	switch tray.Type {
	case devicetypes.ComponentTypeCompute:
		request.Target = &corev1.GetComponentFirmwareDetailsRequest_MachineIds{MachineIds: &corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: tray.ComponentID}}}}
	case devicetypes.ComponentTypeNVSwitch:
		request.Target = &corev1.GetComponentFirmwareDetailsRequest_SwitchIds{SwitchIds: &corev1.SwitchIdList{Ids: []*corev1.SwitchId{{Id: tray.ComponentID}}}}
	case devicetypes.ComponentTypePowerShelf:
		request.Target = &corev1.GetComponentFirmwareDetailsRequest_PowerShelfIds{PowerShelfIds: &corev1.PowerShelfIdList{Ids: []*corev1.PowerShelfId{{Id: tray.ComponentID}}}}
	}
	response, err := rs.firmwareReader.GetComponentFirmwareDetails(ctx, request)
	if ctx.Err() != nil {
		return unavailableCoreFirmwareDetails(firmwareDetailsReadError(ctx.Err()))
	}
	if err != nil {
		log.Warn().Str("component_id", tray.Info.ID.String()).Str("grpc_status_code", status.Code(err).String()).Msg("Firmware details Core lookup failed")
		return unavailableCoreFirmwareDetails(firmwareDetailsReadError(err))
	}
	entries := response.GetEntries()
	if len(entries) != 1 || !sameCoreComponentIdentity(tray.ComponentID, entries[0].GetComponentId()) {
		log.Warn().Str("component_id", tray.Info.ID.String()).Int("entry_count", len(entries)).Msg("Firmware details Core response did not match the requested component")
		return unavailableCoreFirmwareDetails(pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE)
	}
	entry := entries[0]
	actual, desired, actions := unavailableCoreFirmwareDetails(pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED)
	if entry.GetActual().GetResult() != nil {
		actual.Result = &pb.FirmwareActualDetails_Value{Value: entry.Actual}
	}
	if entry.GetDesired().GetResult() != nil {
		desired.Result = &pb.FirmwareDesiredDetails_Value{Value: entry.Desired}
	}
	if entry.GetActions().GetStored() != nil && entry.GetActions().GetBackend() != nil {
		actions.Result = &pb.FirmwareActionDetails_Value{Value: entry.Actions}
	}
	return actual, desired, actions
}

// unavailableCoreFirmwareDetails fills every Core slot so one later successful
// section can replace its own error without erasing other availability.
func unavailableCoreFirmwareDetails(code pb.FirmwareDetailsReadError) (*pb.FirmwareActualDetails, *pb.FirmwareDesiredDetails, *pb.FirmwareActionDetails) {
	return &pb.FirmwareActualDetails{Result: &pb.FirmwareActualDetails_Error{Error: code}},
		&pb.FirmwareDesiredDetails{Result: &pb.FirmwareDesiredDetails_Error{Error: code}},
		&pb.FirmwareActionDetails{Result: &pb.FirmwareActionDetails_Error{Error: code}}
}

// readFlowFirmwareTasks uses the complete bounded rack selection before filtering
// by recorded UUID/type. A firmware shelf task does not imply rack-wide effects.
func (rs *FlowServerImpl) readFlowFirmwareTasks(ctx context.Context, tray *component.Component) *pb.FirmwareTaskDetailsList {
	result := &pb.FirmwareTaskDetailsList{}
	if tray.RackID == uuid.Nil {
		result.Error = pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED.Enum()
		return result
	}
	tasks, err := rs.taskStore.ListFirmwareTasksForRack(ctx, tray.RackID)
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	if err != nil {
		result.Error = firmwareDetailsReadError(err).Enum()
		return result
	}
	var selected []taskdef.FirmwareTaskDetails
	for _, task := range tasks {
		if !slices.Contains(task.Attributes.ComponentsByType[tray.Type], tray.Info.ID) {
			continue
		}
		entry, convertErr := firmwareTaskDetailsToProto(task)
		if convertErr != nil {
			return &pb.FirmwareTaskDetailsList{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()}
		}
		selected = append(selected, task)
		result.Entries = append(result.Entries, entry)
	}
	rs.addFirmwareTaskWaitingContext(ctx, tray.RackID, selected, result.Entries)
	return result
}

// firmwareDetailsReadError publishes only safe categories, retaining deadline,
// count and missing-identity distinctions without dependency diagnostic text.
func firmwareDetailsReadError(err error) pb.FirmwareDetailsReadError {
	var taskLimit *taskcommon.FirmwareTaskLimitError
	var contextLimit *taskcommon.TaskContextLimitError
	switch {
	case errors.Is(err, errTaskIdentityNotLinked):
		return pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED
	case errors.Is(err, errTaskIdentityInvalid):
		return pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE
	case errors.Is(err, errTaskIdentityLimit), errors.As(err, &taskLimit), errors.As(err, &contextLimit):
		return pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED
	case errors.Is(err, context.DeadlineExceeded), status.Code(err) == codes.DeadlineExceeded:
		return pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED
	case status.Code(err) == codes.Unimplemented:
		return pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED
	default:
		return pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED
	}
}

// boundFirmwareDetailsResponse removes optional references/identities before the
// primary task section, remeasuring the real proxy encoding at every step.
func boundFirmwareDetailsResponse(response *pb.GetTrayFirmwareDetailsResponse) error {
	for attempt := range 4 {
		fits, err := detailsResponseFits(response)
		if err != nil || fits {
			return err
		}
		code := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED
		tasks := response.GetDetails().GetOperations().GetTasks()
		switch attempt {
		case 0:
			for _, task := range tasks.GetEntries() {
				waiting := task.GetWaitingContext()
				if waiting != nil && waiting.Error == nil && (len(waiting.ActiveConflicts) > 0 || len(waiting.EarlierWaiting) > 0) {
					task.WaitingContext = &pb.FirmwareTaskWaitingContext{Error: &code}
				}
			}
		case 1:
			for _, task := range tasks.GetEntries() {
				if task.ExternalTargets != nil && task.ExternalTargets.Error == nil {
					task.ExternalTargets = &pb.FirmwareTaskExternalTargets{Error: &code}
				}
			}
		case 2:
			if tasks.Error == nil {
				response.Details.Operations.Tasks = &pb.FirmwareTaskDetailsList{Error: &code}
			}
		}
	}
	return status.Error(codes.ResourceExhausted, "firmware details exceed the response size limit")
}
