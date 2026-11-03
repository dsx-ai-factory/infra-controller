// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// readPowerDetailsRackID recovers a retained rack identity when the selector's
// active-only Rack relation omitted a soft-deleted rack. Read this after the
// tasks so a failed fallback does not discard already available power intent.
func (rs *FlowServerImpl) readPowerDetailsRackID(ctx context.Context, tray *component.Component) (string, *pb.PowerDetailsReadError) {
	if tray.RackID == uuid.Nil {
		return "", pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()
	}
	if tray.RackExternalID != "" {
		return tray.RackExternalID, nil
	}
	racks, err := rs.inventoryManager.GetRacksByIDsIncludingDeleted(ctx, []uuid.UUID{tray.RackID}, false)
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	if err != nil {
		logPowerIdentityReadFailure(ctx, tray.RackID.String(), "rack_identity_read", err)
		return "", powerDetailsReadError(err).Enum()
	}
	if len(racks) == 0 {
		return "", pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()
	}
	if len(racks) != 1 || racks[0] == nil || racks[0].Info.ID != tray.RackID {
		return "", pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()
	}
	if racks[0].ExternalID == "" {
		return "", pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()
	}
	return racks[0].ExternalID, nil
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
// Callers select relevant tasks before this lookup so unrelated targets add no
// database work. All entries must refer to the same recorded rack, whose
// external ID is supplied by the caller.
func (rs *FlowServerImpl) addPowerTaskExternalTargets(ctx context.Context, externalRackID string, entries []*pb.PowerTaskDetails) {
	// UUID text alone is a lower bound on the encoded response. If it cannot
	// fit, skip the extra query and allocations; the final response-size check
	// will replace the task section with LimitExceeded.
	const canonicalUUIDTextBytes = 36 // uuid.String() includes four separators.
	targetCount := 0
	for _, entry := range entries {
		for _, group := range entry.GetTargets().GetGroups() {
			targetCount += len(group.GetComponentIds())
		}
	}
	if targetCount > powerDetailsResponseBytes/canonicalUUIDTextBytes {
		for _, entry := range entries {
			entry.ExternalTargets = &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum()}
		}
		return
	}

	var ids []uuid.UUID
	seen := make(map[uuid.UUID]struct{})
	for _, entry := range entries {
		for _, group := range entry.GetTargets().GetGroups() {
			for _, recordedID := range group.GetComponentIds() {
				id, err := uuid.Parse(recordedID.GetId())
				if err != nil {
					entry.ExternalTargets = &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()}
					continue
				}
				_, found := seen[id]
				if !found {
					seen[id] = struct{}{}
					ids = append(ids, id)
				}
			}
		}
	}

	identitiesByID := make(map[string]component.Identity)
	var lookupErr error
	if externalRackID != "" && len(ids) > 0 {
		var identities []component.Identity
		identities, lookupErr = rs.inventoryManager.GetComponentIdentitiesByIDsIncludingDeleted(ctx, ids)
		if ctx.Err() != nil {
			lookupErr = ctx.Err()
		}
		if lookupErr != nil {
			logPowerIdentityReadFailure(ctx, entries[0].GetRackId().GetId(), "component_identity_read", lookupErr)
		}
		for _, identity := range identities {
			identitiesByID[identity.ID.String()] = identity
		}
	}

	for _, entry := range entries {
		if entry.ExternalTargets != nil {
			continue
		}
		if lookupErr != nil {
			code := powerDetailsReadError(lookupErr)
			entry.ExternalTargets = &pb.PowerTaskExternalTargets{Error: &code}
			continue
		}
		entry.ExternalTargets = powerTaskExternalTargets(externalRackID, entry.GetTargets(), identitiesByID)
	}
}

// powerTaskExternalTargets preserves the order and membership of the existing
// targets field rather than sorting by mutable inventory identifiers. A missing
// or mismatched identity makes only this task's external selection unavailable;
// a partial selection could make a power operation appear to affect fewer trays.
func powerTaskExternalTargets(externalRackID string, targets *pb.ComponentsByType, identities map[string]component.Identity) *pb.PowerTaskExternalTargets {
	if externalRackID == "" {
		return &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()}
	}
	result := &pb.PowerTaskExternalTargets{RackId: externalRackID}
	for _, group := range targets.GetGroups() {
		for _, id := range group.GetComponentIds() {
			identity, found := identities[id.GetId()]
			if !found || identity.ExternalID == "" {
				return &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()}
			}
			if group.GetType() == pb.ComponentType_COMPONENT_TYPE_UNKNOWN || protobuf.ComponentTypeTo(identity.Type) != group.GetType() {
				return &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()}
			}
			result.Targets = append(result.Targets, &pb.ExternalRef{Type: group.GetType(), Id: identity.ExternalID})
		}
	}
	return result
}
