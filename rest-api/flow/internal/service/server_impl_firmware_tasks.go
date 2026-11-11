// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// firmwareTaskDetailsToProto exposes only the store's sanitized intent projection.
// Requested targets remain separate from effective Core expectations and recorded
// execution membership; no updater payload or execution report is reconstructed.
func firmwareTaskDetailsToProto(task taskdef.FirmwareTaskDetails) (*pb.FirmwareTaskDetails, error) {
	operation, err := protobuf.FirmwareControlOperationTo(task.Operation)
	if err != nil {
		return nil, err
	}
	entry := &pb.FirmwareTaskDetails{
		Id:                     protobuf.UUIDTo(task.ID),
		RackId:                 protobuf.UUIDTo(task.RackID),
		Operation:              operation,
		Targets:                protobuf.ComponentsByTypeTo(task.Attributes.ComponentsByType),
		Status:                 protobuf.TaskStatusTo(task.Status),
		CreatedAt:              timestamppb.New(task.CreatedAt),
		UpdatedAt:              timestamppb.New(task.UpdatedAt),
		SubTargets:             task.SubTargets,
		OverrideReadinessCheck: task.OverrideReadinessCheck,
		OverrideVersionCheck:   task.OverrideVersionCheck,
		RequestedTargets:       &pb.FirmwareRequestedTargets{},
	}
	if task.StartedAt != nil {
		entry.StartedAt = timestamppb.New(*task.StartedAt)
	}
	if task.StartTime != 0 {
		entry.StartTime = &task.StartTime
	}
	if task.EndTime != 0 {
		entry.EndTime = &task.EndTime
	}
	if task.RequestedTargets.Shared != nil {
		entry.RequestedTargets.Selection = &pb.FirmwareRequestedTargets_Shared{Shared: firmwareTargetMetadataToProto(*task.RequestedTargets.Shared)}
	} else {
		typed := &pb.FirmwareTargetsByType{}
		// Match the existing protobuf ComponentType order without map iteration order.
		for _, kind := range []devicetypes.ComponentType{devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch, devicetypes.ComponentTypePowerShelf} {
			target, found := task.RequestedTargets.ByType[kind]
			if found {
				typed.Entries = append(typed.Entries, &pb.FirmwareTargetForType{
					ComponentType: protobuf.ComponentTypeTo(kind),
					Target:        firmwareTargetMetadataToProto(target),
				})
			}
		}
		entry.RequestedTargets.Selection = &pb.FirmwareRequestedTargets_ByType{ByType: typed}
	}
	return entry, nil
}

// firmwareTargetMetadataToProto preserves exact display metadata already curated
// by the store. Unavailable/default targets never acquire a fabricated value.
func firmwareTargetMetadataToProto(target taskdef.FirmwareTargetMetadata) *pb.FirmwareTargetMetadata {
	result := &pb.FirmwareTargetMetadata{Kind: pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_UNAVAILABLE}
	switch target.Kind {
	case taskdef.FirmwareTargetConfiguredDefault:
		result.Kind = pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_CONFIGURED_DEFAULT
	case taskdef.FirmwareTargetVersion:
		result.Kind = pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_VERSION
		result.Value = target.Value
	case taskdef.FirmwareTargetManifest:
		result.Kind = pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_MANIFEST
		result.Value = target.Value
	}
	return result
}

// addFirmwareTaskWaitingContext makes one bounded metadata read for all selected
// waiters. Failure leaves their primary records and requested targets visible.
func (rs *FlowServerImpl) addFirmwareTaskWaitingContext(ctx context.Context, rackID uuid.UUID, tasks []taskdef.FirmwareTaskDetails, entries []*pb.FirmwareTaskDetails) {
	hasWaiting := slices.ContainsFunc(tasks, func(task taskdef.FirmwareTaskDetails) bool { return task.Status == taskcommon.TaskStatusWaiting })
	if !hasWaiting {
		return
	}
	metadata, err := rs.taskStore.ListTaskContextForRack(ctx, rackID)
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	for i, task := range tasks {
		if task.Status != taskcommon.TaskStatusWaiting {
			continue
		}
		if err != nil {
			entries[i].WaitingContext = &pb.FirmwareTaskWaitingContext{Error: firmwareDetailsReadError(err).Enum()}
			continue
		}
		active, earlier := taskWaitingReferences(taskdef.TaskContextMetadata{
			ID:         task.ID,
			RackID:     task.RackID,
			Type:       taskcommon.TaskTypeFirmwareControl,
			Attributes: task.Attributes,
			CreatedAt:  task.CreatedAt,
		}, metadata)
		entries[i].WaitingContext = &pb.FirmwareTaskWaitingContext{ActiveConflicts: active, EarlierWaiting: earlier}
	}
}

// addFirmwareTaskExternalTargets resolves retained inventory identities without
// changing recorded selections. One unavailable identity fails its whole task.
func (rs *FlowServerImpl) addFirmwareTaskExternalTargets(ctx context.Context, tray *component.Component, details *pb.TrayFirmwareDetails) {
	rackID, rackErr := rs.readDetailsRackID(ctx, tray)
	if rackID != "" {
		details.RackExternalId = &rackID
	}
	if rackErr != nil {
		if !errors.Is(rackErr, errTaskIdentityNotLinked) && !errors.Is(rackErr, errTaskIdentityInvalid) {
			logFirmwareIdentityReadFailure(ctx, tray.RackID.String(), "rack_identity_read", rackErr)
		}
		for _, task := range details.Operations.Tasks.Entries {
			task.ExternalTargets = &pb.FirmwareTaskExternalTargets{Error: firmwareDetailsReadError(rackErr).Enum()}
		}
		return
	}
	results, identityErr := readTaskInventoryTargets(ctx, rs, rackID, details.Operations.Tasks.Entries)
	if identityErr != nil {
		logFirmwareIdentityReadFailure(ctx, tray.RackID.String(), "component_identity_read", identityErr)
	}
	for i, result := range results {
		external := &pb.FirmwareTaskExternalTargets{RackId: result.rackID, Targets: result.targets}
		if result.err != nil {
			external.Error = firmwareDetailsReadError(result.err).Enum()
		}
		details.Operations.Tasks.Entries[i].ExternalTargets = external
	}
}

// logFirmwareIdentityReadFailure keeps useful query context while excluding
// scanned values, SQL text and dependency diagnostics. Caller expiry is routine.
func logFirmwareIdentityReadFailure(ctx context.Context, rackID, stage string, err error) {
	if ctx.Err() != nil {
		return
	}
	event := log.Warn().Ctx(ctx).Str("rack_id", rackID).Str("failure_stage", stage).
		Str("error_category", firmwareDetailsReadError(err).String()).Str("error_type", fmt.Sprintf("%T", err))
	var pgErr *pgconn.PgError
	if errors.As(err, &pgErr) {
		event = event.Str("sqlstate", pgErr.Code)
	}
	event.Msg("Firmware inventory identities could not be read")
}
