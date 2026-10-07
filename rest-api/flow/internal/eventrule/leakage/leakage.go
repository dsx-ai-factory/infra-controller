// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakage

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
)

// TypeHardwareLeakDetected identifies an observed hardware coolant leak.
const TypeHardwareLeakDetected eventrule.Type = "hardware.leak.detected"

// defaultRuleID is the stable identity of the immutable leakage fallback.
var defaultRuleID = uuid.MustParse("f34b87f7-cb1b-4b08-aa51-30c0b3b58680")

// InventoryResolver is the canonical inventory resolution capability required
// by leakage target strategies.
type InventoryResolver interface {
	// RackByID returns a non-nil canonical rack whose ID matches the requested
	// ID, or an error when the rack cannot be resolved.
	RackByID(context.Context, uuid.UUID, bool) (*rack.Rack, error)
}

type targetResolver struct {
	inventory InventoryResolver
}

// RegisterTargetResolvers registers leakage-specific target behavior.
func RegisterTargetResolvers(
	registry *target.Registry,
	inventory InventoryResolver,
) error {
	if inventory == nil {
		return fmt.Errorf("leakage target inventory is required")
	}

	resolver := &targetResolver{inventory: inventory}
	return registry.Register(
		TypeHardwareLeakDetected,
		eventrule.TargetStrategyAffectedComponents,
		resolver,
	)
}

func (r *targetResolver) Resolve(
	ctx context.Context,
	request target.ResolveRequest,
) ([]target.Target, error) {
	resource := request.Resource
	switch resource.Kind {
	case eventrule.ResourceKindComponent:
		return r.resolveAffectedComponentsInRack(ctx, resource)
	case eventrule.ResourceKindRack:
		resolved := target.Target{
			Kind:   eventrule.ResourceKindRack,
			ID:     resource.RackID,
			RackID: resource.RackID,
		}
		return []target.Target{resolved}, nil
	default:
		return nil, fmt.Errorf(
			"%w: "+
				"leakage affected-components strategy does not support resource kind %q",
			target.ErrUnresolvable,
			resource.Kind,
		)
	}
}

func (r *targetResolver) resolveAffectedComponentsInRack(
	ctx context.Context,
	resource eventrule.ResolvedResource,
) ([]target.Target, error) {
	resolvedRack, err := r.inventory.RackByID(ctx, resource.RackID, true)
	if err != nil {
		return nil, fmt.Errorf("resolve leakage rack %s: %w", resource.RackID, err)
	}

	ids, err := affectedComponentIDs(resolvedRack, resource.ID)
	if err != nil {
		return nil, err
	}

	targets := make([]target.Target, 0, len(ids))
	for _, id := range ids {
		resolved := target.Target{
			Kind:   eventrule.ResourceKindComponent,
			ID:     id,
			RackID: resource.RackID,
		}
		targets = append(targets, resolved)
	}

	return targets, nil
}

// affectedComponentIDs isolates topology selection from event resolution and
// target conversion. It models leak impact using rack slot ordering.
// TODO(topology-provider integration): Replace this helper with the topology
// provider's leakage-impact resolution.
func affectedComponentIDs(resolvedRack *rack.Rack, sourceID uuid.UUID) ([]uuid.UUID, error) {
	sourceSlot := -1
	for _, candidate := range resolvedRack.Components {
		if candidate.Info.ID == sourceID {
			sourceSlot = candidate.Position.SlotID
			break
		}
	}
	if sourceSlot < 0 {
		return nil, fmt.Errorf(
			"%w: "+
				"leaking component %s has no valid position in rack %s topology",
			target.ErrUnresolvable,
			sourceID,
			resolvedRack.Info.ID,
		)
	}

	ids := make([]uuid.UUID, 0, len(resolvedRack.Components))
	for _, candidate := range resolvedRack.Components {
		candidateSlot := candidate.Position.SlotID
		if candidateSlot < 0 ||
			(candidate.Info.ID != sourceID && candidateSlot >= sourceSlot) {
			continue
		}

		ids = append(ids, candidate.Info.ID)
	}

	slices.SortFunc(ids, func(a, b uuid.UUID) int {
		return cmp.Compare(a.String(), b.String())
	})
	return ids, nil
}

// DefaultRule returns the immutable safety fallback for leakage events.
func DefaultRule() *eventrule.Rule {
	return &eventrule.Rule{
		ID:          defaultRuleID,
		Origin:      eventrule.RuleOriginBuiltIn,
		Name:        "Default leakage response",
		Description: "Power off components affected by a detected hardware leak.",
		Enabled:     true,
		EventType:   TypeHardwareLeakDetected,
		Policy: eventrule.Policy{
			Actions: []eventrule.Action{
				{
					Name: "power_off_affected_components",
					Spec: &eventrule.SubmitTask{
						Operation: &operations.PowerControlTaskInfo{
							Operation: operations.PowerOperationForcePowerOff,
						},
						TargetStrategy:   eventrule.TargetStrategyAffectedComponents,
						ConflictStrategy: eventrule.ConflictStrategyQueue,
						Description:      "Leakage response",
					},
				},
			},
		},
	}
}
