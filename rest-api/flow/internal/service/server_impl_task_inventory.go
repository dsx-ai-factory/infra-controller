// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// These local failures describe inventory evidence, independently of the power
// and firmware wire enums. Dependency errors retain their type until conversion.
var (
	errTaskIdentityNotLinked = errors.New("task inventory identity is missing")
	errTaskIdentityInvalid   = errors.New("task inventory identity is inconsistent")
	errTaskIdentityLimit     = errors.New("task inventory identities exceed the response allowance")
)

// taskInventoryTargets is a complete inventory selection or a failure. A partial
// selection could make a disruptive task appear to affect fewer components.
type taskInventoryTargets struct {
	rackID  string
	targets []*pb.ExternalRef
	err     error
}

// recordedTaskTargets admits the two task-detail messages without coupling their
// operation payloads or error enums. Both preserve the store's UUID/type ordering.
type recordedTaskTargets interface {
	GetTargets() *pb.ComponentsByType
}

// readDetailsRackID recovers retained rack identity when the active relation
// omits a deleted rack. It never substitutes the internal Flow UUID.
func (rs *FlowServerImpl) readDetailsRackID(ctx context.Context, tray *component.Component) (string, error) {
	if tray.RackID == uuid.Nil {
		return "", errTaskIdentityNotLinked
	}
	if tray.RackExternalID != "" {
		return tray.RackExternalID, nil
	}
	racks, err := rs.inventoryManager.GetRacksByIDsIncludingDeleted(ctx, []uuid.UUID{tray.RackID}, false)
	if ctx.Err() != nil {
		return "", ctx.Err()
	}
	if err != nil {
		return "", err
	}
	if len(racks) == 0 {
		return "", errTaskIdentityNotLinked
	}
	if len(racks) != 1 || racks[0] == nil || racks[0].Info.ID != tray.RackID {
		return "", errTaskIdentityInvalid
	}
	if racks[0].ExternalID == "" {
		return "", errTaskIdentityNotLinked
	}
	return racks[0].ExternalID, nil
}

// readTaskInventoryTargets performs one retained-identity query for the selected
// tasks. Missing, purged or mismatched identities fail only their task. Current
// rack membership is irrelevant to what that task recorded. The returned error
// is the shared query failure, for one bounded diagnostic at the caller.
func readTaskInventoryTargets[T recordedTaskTargets](ctx context.Context, rs *FlowServerImpl, externalRackID string, entries []T) ([]taskInventoryTargets, error) {
	results := make([]taskInventoryTargets, len(entries))
	const canonicalUUIDTextBytes = 36 // uuid.String includes four separators.
	targetCount := 0
	for _, entry := range entries {
		for _, group := range entry.GetTargets().GetGroups() {
			targetCount += len(group.GetComponentIds())
		}
	}
	// UUID text alone cannot fit, so avoid the redundant lookup and allocations.
	if targetCount > detailsResponseBytes/canonicalUUIDTextBytes {
		for i := range results {
			results[i].err = errTaskIdentityLimit
		}
		return results, nil
	}
	var ids []uuid.UUID
	seen := make(map[uuid.UUID]struct{})
	for i, entry := range entries {
		for _, group := range entry.GetTargets().GetGroups() {
			for _, recordedID := range group.GetComponentIds() {
				id, err := uuid.Parse(recordedID.GetId())
				if err != nil {
					results[i].err = errTaskIdentityInvalid
					continue
				}
				_, exists := seen[id]
				if !exists {
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
		for _, identity := range identities {
			identitiesByID[identity.ID.String()] = identity
		}
	}
	for i, entry := range entries {
		if results[i].err != nil {
			continue
		}
		if lookupErr != nil {
			results[i].err = lookupErr
			continue
		}
		results[i] = taskInventorySelection(externalRackID, entry.GetTargets(), identitiesByID)
	}
	return results, lookupErr
}

// taskInventorySelection retains complete membership in recorded UUID/type order,
// including empty selections. The type is checked before publishing each identity.
func taskInventorySelection(externalRackID string, targets *pb.ComponentsByType, identities map[string]component.Identity) taskInventoryTargets {
	if externalRackID == "" {
		return taskInventoryTargets{err: errTaskIdentityNotLinked}
	}
	result := taskInventoryTargets{rackID: externalRackID}
	for _, group := range targets.GetGroups() {
		for _, id := range group.GetComponentIds() {
			identity, found := identities[id.GetId()]
			if !found || identity.ExternalID == "" {
				return taskInventoryTargets{err: errTaskIdentityNotLinked}
			}
			if group.GetType() == pb.ComponentType_COMPONENT_TYPE_UNKNOWN || protobuf.ComponentTypeTo(identity.Type) != group.GetType() {
				return taskInventoryTargets{err: errTaskIdentityInvalid}
			}
			result.targets = append(result.targets, &pb.ExternalRef{Type: group.GetType(), Id: identity.ExternalID})
		}
	}
	return result
}
