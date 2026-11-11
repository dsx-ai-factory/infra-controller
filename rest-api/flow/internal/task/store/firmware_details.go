// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	stderrors "errors"
	"fmt"
	"slices"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
)

// ListFirmwareTasksForRack reads complete safe firmware intent in the caller's
// transaction or store connection, retaining recorded selections without inventory.
func (s *PostgresStore) ListFirmwareTasksForRack(ctx context.Context, rackID uuid.UUID) ([]taskdef.FirmwareTaskDetails, error) {
	rows, err := model.ListFirmwareTasksForRack(ctx, s.idb(ctx), rackID)
	if err != nil {
		var limitErr *taskcommon.FirmwareTaskLimitError
		if stderrors.As(err, &limitErr) {
			return nil, limitErr
		}
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		// Driver and scan errors can quote stored JSON, including credentials.
		event := log.Error().Ctx(ctx).Str("rack_id", rackID.String()).
			Str("failure_stage", "database_read").Str("error_type", fmt.Sprintf("%T", err))
		var pgErr *pgconn.PgError
		if stderrors.As(err, &pgErr) {
			event = event.Str("sqlstate", pgErr.Code)
		}
		event.Msg("Failed to read outstanding firmware tasks")
		return nil, errors.GRPCErrorInternal("failed to read outstanding firmware tasks")
	}

	result := make([]taskdef.FirmwareTaskDetails, 0, len(rows))
	for _, row := range rows {
		converted, err := firmwareTaskDetailsFromDAO(row)
		if err != nil {
			return nil, err
		}
		result = append(result, converted)
	}
	return result, nil
}

// firmwareTaskDetailsFromDAO validates the operation before discarding its
// private fields. Unknown operations must not use CodeString's upgrade fallback.
func firmwareTaskDetailsFromDAO(row model.Task) (taskdef.FirmwareTaskDetails, error) {
	logger := log.With().Str("rack_id", row.RackID.String()).Str("task_id", row.ID.String()).Logger()
	decoded, err := operations.New(row.Type, row.Information)
	if err != nil {
		logger.Error().Str("failure_stage", "operation_decode").
			Str("error_type", fmt.Sprintf("%T", err)).Msg("Failed to decode stored firmware operation")
		return taskdef.FirmwareTaskDetails{}, errors.GRPCErrorInternal(fmt.Sprintf("firmware task %s has invalid operation data", row.ID))
	}
	firmware, ok := decoded.(*operations.FirmwareControlTaskInfo)
	if !ok {
		logger.Error().Str("failure_stage", "operation_type").
			Str("decoded_type", fmt.Sprintf("%T", decoded)).Msg("Stored task does not contain a firmware operation")
		return taskdef.FirmwareTaskDetails{}, errors.GRPCErrorInternal(fmt.Sprintf("task %s is not a firmware task", row.ID))
	}
	err = firmware.Validate()
	if err != nil {
		logger.Error().Str("failure_stage", "operation_validation").
			Str("error_type", fmt.Sprintf("%T", err)).Msg("Stored firmware operation failed validation")
		return taskdef.FirmwareTaskDetails{}, errors.GRPCErrorInternal(fmt.Sprintf("firmware task %s has an invalid operation", row.ID))
	}

	// These query rows are privately owned. Sort and deduplicate each UUID
	// group in place, retaining empty groups and every recorded identity.
	for componentType, ids := range row.Attributes.ComponentsByType {
		operation.SortComponentUUIDs(ids)
		row.Attributes.ComponentsByType[componentType] = slices.Compact(ids)
	}
	var requestedTargets taskdef.FirmwareRequestedTargets
	requestedTargets.FromTargetVersion(firmware.TargetVersion)
	return taskdef.FirmwareTaskDetails{
		ID:                     row.ID,
		RackID:                 row.RackID,
		Operation:              firmware.Operation,
		RequestedTargets:       requestedTargets,
		Attributes:             row.Attributes,
		SubTargets:             firmware.SubTargets,
		OverrideReadinessCheck: firmware.OverrideReadinessCheck,
		OverrideVersionCheck:   firmware.OverrideVersionCheck,
		StartTime:              firmware.StartTime,
		EndTime:                firmware.EndTime,
		Status:                 row.Status,
		CreatedAt:              row.CreatedAt,
		UpdatedAt:              row.UpdatedAt,
		StartedAt:              row.StartedAt,
	}, nil
}
