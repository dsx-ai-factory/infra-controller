// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// readPowerDetailsRackID recovers a retained rack identity when the selector's
// active-only Rack relation omitted a soft-deleted rack. Read this after the
// tasks so a failed fallback does not discard already available power intent.
func (rs *FlowServerImpl) readPowerDetailsRackID(ctx context.Context, tray *component.Component) (string, *pb.PowerDetailsReadError) {
	id, err := rs.readDetailsRackID(ctx, tray)
	if err != nil {
		if !errors.Is(err, errTaskIdentityNotLinked) && !errors.Is(err, errTaskIdentityInvalid) {
			logPowerIdentityReadFailure(ctx, tray.RackID.String(), "rack_identity_read", err)
		}
		return "", powerDetailsReadError(err).Enum()
	}
	return id, nil
}

// logPowerIdentityReadFailure identifies the failed read without exposing SQL
// details or scanned values. Routine caller expiry needs no additional warning.
func logPowerIdentityReadFailure(ctx context.Context, rackID, stage string, err error) {
	if ctx.Err() != nil {
		return
	}
	event := log.Warn().Ctx(ctx).
		Str("rack_id", rackID).
		Str("failure_stage", stage).
		Str("error_category", powerDetailsReadError(err).String()).
		Str("error_type", fmt.Sprintf("%T", err))
	var pgErr *pgconn.PgError
	if errors.As(err, &pgErr) {
		event = event.Str("sqlstate", pgErr.Code)
	}
	event.Msg("Power inventory identities could not be read")
}

// addPowerTaskExternalTargets resolves saved UUIDs once for REST consumers,
// which identify trays by inventory ID. The lookup includes deleted components
// and ignores current rack membership; neither changes what the task selected.
// Callers pass their selected tasks together to avoid per-tray lookups.
// All entries must refer to the same recorded rack, whose
// external ID is supplied by the caller.
func (rs *FlowServerImpl) addPowerTaskExternalTargets(ctx context.Context, externalRackID string, entries []*pb.PowerTaskDetails) {
	results, err := readTaskInventoryTargets(ctx, rs, externalRackID, entries)
	if err != nil {
		logPowerIdentityReadFailure(ctx, entries[0].GetRackId().GetId(), "component_identity_read", err)
	}
	for i, result := range results {
		entries[i].ExternalTargets = &pb.PowerTaskExternalTargets{RackId: result.rackID, Targets: result.targets}
		if result.err != nil {
			entries[i].ExternalTargets.Error = powerDetailsReadError(result.err).Enum()
		}
	}
}
