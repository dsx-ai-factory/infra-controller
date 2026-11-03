// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/base32"
	"encoding/json"
	"errors"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// powerDetailsReadTimeout leaves ten seconds in the proxy's 40-second
	// activity budget for transport and response encoding.
	powerDetailsReadTimeout = 30 * time.Second
	// powerDetailsResponseReserve leaves assembly time before a shorter caller
	// deadline. The entry deadline applies to inventory and both data sources.
	powerDetailsResponseReserve = time.Second
	// powerDetailsResponseBytes bounds this method's encoded proxy result below
	// Temporal's 2 MiB payload ceiling. It uses the numeric allowance from
	// site-workflow/pkg/activity/inventory.go, applied here to the encoded proxy
	// wrapper rather than inventory's batching calculation.
	// The task count bound alone cannot limit recorded target arrays.
	powerDetailsResponseBytes = 1945 * 1024
)

// componentPowerReader borrows the provider's existing connection; power detail
// requests neither create a Core client nor take ownership of its lifetime.
type componentPowerReader interface {
	GetComponentPowerDetails(context.Context, *corev1.GetComponentPowerDetailsRequest) (*corev1.GetComponentPowerDetailsResponse, error)
}

// powerReaderFromRegistry permits deployments without the NICo provider to keep
// serving Flow task details. A configured provider lends its existing client.
func powerReaderFromRegistry(registry *providerapi.ProviderRegistry) componentPowerReader {
	provider, ok := registry.Get(nicoprovider.ProviderName).(*nicoprovider.Provider)
	if !ok || provider == nil {
		return nil
	}
	return provider.Client()
}

// GetTrayPowerDetails resolves public tray identity before independently reading
// Core facts and Flow tasks. Neither failure cancels the other source; caller
// cancellation still fails the RPC after both child calls have returned.
func (rs *FlowServerImpl) GetTrayPowerDetails(
	ctx context.Context,
	req *pb.GetTrayPowerDetailsRequest,
) (*pb.GetTrayPowerDetailsResponse, error) {
	deadline := time.Now().Add(powerDetailsReadTimeout)
	callerDeadline, hasDeadline := ctx.Deadline()
	if hasDeadline && callerDeadline.Add(-powerDetailsResponseReserve).Before(deadline) {
		deadline = callerDeadline.Add(-powerDetailsResponseReserve)
	}
	readCtx, cancel := context.WithDeadline(ctx, deadline)
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
			log.Warn().Str("grpc_code", status.Code(err).String()).Msg("Power details inventory lookup failed")
			code := codes.Unavailable
			if status.Code(err) == codes.Internal {
				code = codes.Internal
			}
			return nil, status.Error(code, "tray inventory could not be read")
		}
	}
	if tray.Type != devicetypes.ComponentTypeCompute &&
		tray.Type != devicetypes.ComponentTypeNVSwitch &&
		tray.Type != devicetypes.ComponentTypePowerShelf {
		return nil, status.Error(codes.InvalidArgument, "component type does not support power details")
	}

	details := &pb.TrayPowerDetails{
		ComponentType: protobuf.ComponentTypeTo(tray.Type),
		Operations:    &pb.PowerOperations{},
	}
	if tray.ComponentID != "" {
		details.ComponentId = &tray.ComponentID
	}
	if tray.RackID != uuid.Nil {
		details.RackId = protobuf.UUIDTo(tray.RackID)
	}

	// Each goroutine writes distinct fields and is joined before serialization.
	// A task-store error must not hide Core facts, or vice versa.
	var reads sync.WaitGroup
	reads.Add(2)
	go func() {
		defer reads.Done()
		details.Actual, details.Desired, details.Operations.Actions = rs.readCorePowerDetails(readCtx, tray)
	}()
	go func() {
		defer reads.Done()
		details.Operations.Tasks = rs.readFlowPowerTasks(readCtx, tray)
		externalRackID, rackError := rs.readPowerDetailsRackID(readCtx, tray)
		if externalRackID != "" {
			details.RackExternalId = &externalRackID
		}
		if rackError != nil {
			for _, entry := range details.Operations.Tasks.Entries {
				entry.ExternalTargets = &pb.PowerTaskExternalTargets{Error: rackError}
			}
			return
		}
		rs.addPowerTaskExternalTargets(readCtx, externalRackID, details.Operations.Tasks.Entries)
	}()
	reads.Wait()

	response := &pb.GetTrayPowerDetailsResponse{Details: details}
	err = boundPowerDetailsResponse(response)
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if err != nil {
		return nil, err
	}
	return response, nil
}

// readCorePowerDetails checks the single requested managed identity before using
// any returned section. Missing sections remain failures instead of zero values.
func (rs *FlowServerImpl) readCorePowerDetails(
	ctx context.Context,
	tray *component.Component,
) (*pb.PowerActualDetails, *pb.PowerDesiredDetails, *pb.PowerActionDetails) {
	if tray.ComponentID == "" {
		return unavailableCorePowerDetails(pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED)
	}
	if rs.powerReader == nil {
		return unavailableCorePowerDetails(pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED)
	}

	request := &corev1.GetComponentPowerDetailsRequest{}
	switch tray.Type {
	case devicetypes.ComponentTypeCompute:
		request.Target = &corev1.GetComponentPowerDetailsRequest_MachineIds{
			MachineIds: &corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: tray.ComponentID}}},
		}
	case devicetypes.ComponentTypeNVSwitch:
		request.Target = &corev1.GetComponentPowerDetailsRequest_SwitchIds{
			SwitchIds: &corev1.SwitchIdList{Ids: []*corev1.SwitchId{{Id: tray.ComponentID}}},
		}
	case devicetypes.ComponentTypePowerShelf:
		request.Target = &corev1.GetComponentPowerDetailsRequest_PowerShelfIds{
			PowerShelfIds: &corev1.PowerShelfIdList{Ids: []*corev1.PowerShelfId{{Id: tray.ComponentID}}},
		}
	}
	response, err := rs.powerReader.GetComponentPowerDetails(ctx, request)
	if ctx.Err() != nil {
		return unavailableCorePowerDetails(powerDetailsReadError(ctx.Err()))
	}
	if err != nil {
		log.Warn().Str("component_id", tray.Info.ID.String()).
			Str("grpc_code", status.Code(err).String()).Msg("Power details Core lookup failed")
		return unavailableCorePowerDetails(powerDetailsReadError(err))
	}
	entries := response.GetEntries()
	if len(entries) != 1 || !sameCorePowerIdentity(tray.ComponentID, entries[0].GetComponentId()) {
		log.Warn().Str("component_id", tray.Info.ID.String()).
			Int("entry_count", len(entries)).Msg("Power details Core response did not match the requested component")
		return unavailableCorePowerDetails(pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE)
	}

	entry := entries[0]
	actual, desired, actions := unavailableCorePowerDetails(pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE)
	if entry.GetActual().GetOutcome() != nil {
		actual.Result = &pb.PowerActualDetails_Value{Value: entry.Actual}
	}
	if entry.Desired != nil {
		desired.Result = &pb.PowerDesiredDetails_Value{Value: entry.Desired}
	}
	if entry.Actions == nil {
		actions.Result = &pb.PowerActionDetails_Error{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED}
	} else {
		actions.Result = &pb.PowerActionDetails_Value{Value: entry.Actions}
	}
	return actual, desired, actions
}

// sameCorePowerIdentity accepts Core's canonical lowercase hardware hash while
// preserving its seven-byte typed prefix. Flow can store an uppercase hash from
// ingestion; Core parses that Base32Hex suffix case-insensitively. Exact matches
// also support identifiers outside this 59-byte representation.
func sameCorePowerIdentity(expected, observed string) bool {
	if expected == observed {
		return true
	}
	if len(expected) != 59 || len(observed) != 59 || expected[:7] != observed[:7] {
		return false
	}
	hash := strings.ToUpper(expected[7:])
	if hash != strings.ToUpper(observed[7:]) {
		return false
	}
	encoding := base32.HexEncoding.WithPadding(base32.NoPadding)
	decoded, err := encoding.DecodeString(hash)
	// Round-tripping rejects unused trailing bits that DecodeString discards.
	return err == nil && len(decoded) == 32 && encoding.EncodeToString(decoded) == hash
}

// unavailableCorePowerDetails initializes three failed section slots. A caller
// can replace each slot independently when Core supplies that section.
func unavailableCorePowerDetails(code pb.PowerDetailsReadError) (*pb.PowerActualDetails, *pb.PowerDesiredDetails, *pb.PowerActionDetails) {
	return &pb.PowerActualDetails{Result: &pb.PowerActualDetails_Error{Error: code}},
		&pb.PowerDesiredDetails{Result: &pb.PowerDesiredDetails_Error{Error: code}},
		&pb.PowerActionDetails{Result: &pb.PowerActionDetails_Error{Error: code}}
}

// readFlowPowerTasks filters the bounded rack read by recorded target membership.
// A shelf selection also affects other trays on that rack without selecting them.
// Inventory ID enrichment follows this read so a failed identity lookup cannot
// hide the original tasks or query targets that were filtered out here.
func (rs *FlowServerImpl) readFlowPowerTasks(ctx context.Context, tray *component.Component) *pb.PowerTaskDetailsList {
	tasks, result := rs.readRackPowerTasks(ctx, tray.RackID)
	if result.Error != nil {
		return result
	}
	entries := result.Entries
	result.Entries = nil
	for i, task := range tasks {
		relevance := task.RelevanceFor(tray.Info.ID, tray.Type)
		if relevance == taskdef.PowerTaskUnrelated {
			continue
		}
		entry := entries[i]
		entry.Relevance = pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED
		if relevance == taskdef.PowerTaskRackAffected {
			entry.Relevance = pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED
		}
		result.Entries = append(result.Entries, entry)
	}
	return result
}

// readRackPowerTasks converts one bounded rack read and attaches waiting context.
// Domain records and protobuf entries retain matching order so the tray caller
// can apply relevance without reconstructing targets from protobuf values.
func (rs *FlowServerImpl) readRackPowerTasks(ctx context.Context, rackID uuid.UUID) ([]taskdef.PowerTaskDetails, *pb.PowerTaskDetailsList) {
	result := &pb.PowerTaskDetailsList{}
	if rackID == uuid.Nil {
		code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED
		result.Error = &code
		return nil, result
	}
	tasks, err := rs.taskStore.ListPowerTasksForRack(ctx, rackID)
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	if err != nil {
		code := powerDetailsReadError(err)
		var limitError *taskcommon.PowerTaskLimitError
		if errors.As(err, &limitError) {
			code = pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED
		}
		result.Error = &code
		return nil, result
	}

	for _, task := range tasks {
		entry, err := powerTaskDetailsToProto(task)
		if err != nil {
			code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE
			return nil, &pb.PowerTaskDetailsList{Error: &code}
		}
		result.Entries = append(result.Entries, entry)
	}
	rs.addPowerTaskWaitingContext(ctx, rackID, tasks, result.Entries)
	return tasks, result
}

// powerTaskDetailsToProto retains recorded targets and timestamps without
// assigning tray relevance, which depends on the member being displayed.
func powerTaskDetailsToProto(task taskdef.PowerTaskDetails) (*pb.PowerTaskDetails, error) {
	operation, err := protobuf.TaskOperationTo(&task.Operation)
	if err != nil {
		return nil, err
	}
	entry := &pb.PowerTaskDetails{
		Id:        protobuf.UUIDTo(task.ID),
		RackId:    protobuf.UUIDTo(task.RackID),
		Operation: operation,
		Targets:   protobuf.ComponentsByTypeTo(task.Attributes.ComponentsByType),
		Status:    protobuf.TaskStatusTo(task.Status),
		CreatedAt: timestamppb.New(task.CreatedAt),
		UpdatedAt: timestamppb.New(task.UpdatedAt),
	}
	if task.StartedAt != nil {
		entry.StartedAt = timestamppb.New(*task.StartedAt)
	}
	return entry, nil
}

// powerDetailsReadError exposes a bounded category instead of dependency text.
func powerDetailsReadError(err error) pb.PowerDetailsReadError {
	if errors.Is(err, context.DeadlineExceeded) || status.Code(err) == codes.DeadlineExceeded {
		return pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED
	}
	if status.Code(err) == codes.Unimplemented {
		return pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED
	}
	return pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED
}

// boundPowerDetailsResponse drops waiting-context references, then external
// identities, before making the task list unavailable. Optional information
// must not hide the original power-task records.
// Selected targets are never truncated. JSON wrapper encoding matters because
// its HTML escaping can expand strings beyond protojson's size.
func boundPowerDetailsResponse(response *pb.GetTrayPowerDetailsResponse) error {
	for attempt := range 4 {
		message, err := protojson.Marshal(response)
		if err != nil {
			return status.Error(codes.Internal, "power details could not be encoded")
		}
		encoded, err := json.Marshal(grpcproxy.Response{ResponseJSON: message})
		if err != nil {
			return status.Error(codes.Internal, "power details could not be encoded")
		}
		if len(encoded) <= powerDetailsResponseBytes {
			return nil
		}
		code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED
		switch attempt {
		case 0:
			// Keep the primary tasks while removing optional references. Empty
			// contexts and earlier read failures still mean what they did before.
			for _, task := range response.GetDetails().GetOperations().GetTasks().GetEntries() {
				waitingContext := task.GetWaitingContext()
				if waitingContext == nil || waitingContext.Error != nil ||
					(len(waitingContext.ActiveConflicts) == 0 && len(waitingContext.EarlierWaiting) == 0) {
					continue
				}
				task.WaitingContext = &pb.PowerTaskWaitingContext{Error: &code}
			}
		case 1:
			// Retain original task records before giving up on the whole section.
			// Preserve absent support and existing identity lookup failures.
			for _, task := range response.GetDetails().GetOperations().GetTasks().GetEntries() {
				if task.ExternalTargets != nil && task.ExternalTargets.Error == nil {
					task.ExternalTargets = &pb.PowerTaskExternalTargets{Error: &code}
				}
			}
		case 2:
			// If primary tasks still do not fit, retain Core facts and report
			// the task section as unavailable rather than dropping any targets.
			response.Details.Operations.Tasks = &pb.PowerTaskDetailsList{Error: &code}
		}
	}
	return status.Error(codes.ResourceExhausted, "power details exceed the response size limit")
}
