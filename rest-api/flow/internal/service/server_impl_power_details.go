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
		FlowComponentId: protobuf.UUIDTo(tray.Info.ID),
		ComponentType:   protobuf.ComponentTypeTo(tray.Type),
		Operations:      &pb.PowerOperations{},
	}
	if tray.ComponentID != "" {
		details.CoreComponentId = &tray.ComponentID
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
func (rs *FlowServerImpl) readFlowPowerTasks(ctx context.Context, tray *component.Component) *pb.PowerTaskDetailsList {
	result := &pb.PowerTaskDetailsList{}
	if tray.RackID == uuid.Nil {
		code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED
		result.Error = &code
		return result
	}
	tasks, err := rs.taskStore.ListPowerTasksForRack(ctx, tray.RackID)
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
		return result
	}

	for _, task := range tasks {
		relevance := task.RelevanceFor(tray.Info.ID, tray.Type)
		if relevance == taskdef.PowerTaskUnrelated {
			continue
		}
		operation, err := protobuf.TaskOperationTo(&task.Operation)
		if err != nil {
			code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE
			return &pb.PowerTaskDetailsList{Error: &code}
		}
		entry := &pb.PowerTaskDetails{
			Id:        protobuf.UUIDTo(task.ID),
			RackId:    protobuf.UUIDTo(task.RackID),
			Operation: operation,
			Targets:   protobuf.ComponentsByTypeTo(task.Attributes.ComponentsByType),
			Status:    protobuf.TaskStatusTo(task.Status),
			CreatedAt: timestamppb.New(task.CreatedAt),
			UpdatedAt: timestamppb.New(task.UpdatedAt),
			Relevance: pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED,
		}
		if relevance == taskdef.PowerTaskRackAffected {
			entry.Relevance = pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED
		}
		if task.StartedAt != nil {
			entry.StartedAt = timestamppb.New(*task.StartedAt)
		}
		result.Entries = append(result.Entries, entry)
	}
	return result
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

// boundPowerDetailsResponse keeps the whole task list or marks it unavailable;
// it never hides selected targets by truncating a task. JSON wrapper encoding
// matters because its HTML escaping can expand strings beyond protojson's size.
func boundPowerDetailsResponse(response *pb.GetTrayPowerDetailsResponse) error {
	for range 2 {
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
		response.Details.Operations.Tasks = &pb.PowerTaskDetailsList{Error: &code}
	}
	return status.Error(codes.ResourceExhausted, "power details exceed the response size limit")
}
