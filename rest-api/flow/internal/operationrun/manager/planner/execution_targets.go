// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package planner

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"slices"
	"strings"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
)

// executionTargets is a planner-local view over rack execution targets that
// groups the slice operations used while composing a frozen plan.
type executionTargets []operation.RackExecutionTarget

// normalize validates target rows, normalizes component maps, and merges
// duplicate rack rows while preserving the first occurrence of each rack.
func (t executionTargets) normalize() (executionTargets, error) {
	if len(t) == 0 {
		return nil, nil
	}

	normalized := make(executionTargets, 0, len(t))
	indexes := make(map[uuid.UUID]int, len(t))
	for idx, target := range t {
		if target.RackID == uuid.Nil {
			return nil, fmt.Errorf("targets[%d].rack_id is required", idx)
		}

		components, err := target.ComponentsByType.Normalize()
		if err != nil {
			return nil, fmt.Errorf("targets[%d].components_by_type: %w", idx, err)
		}

		if idx, ok := indexes[target.RackID]; ok {
			merged, err := normalized[idx].ComponentsByType.Merge(components)
			if err != nil {
				return nil, fmt.Errorf("targets[%d].components_by_type: %w", idx, err)
			}

			normalized[idx].ComponentsByType = merged
		} else {
			indexes[target.RackID] = len(normalized)
			normalized = append(
				normalized,
				operation.RackExecutionTarget{
					RackID:           target.RackID,
					ComponentsByType: components,
				},
			)
		}
	}

	return normalized, nil
}

// exclude subtracts components from matching rack targets and drops racks that
// have no components left after subtraction.
func (t executionTargets) exclude(
	excluded map[uuid.UUID]operation.ComponentsByType,
) executionTargets {
	if len(t) == 0 || len(excluded) == 0 {
		return t
	}

	result := make(executionTargets, 0, len(t))
	for _, target := range t {
		if excludedComponents, ok := excluded[target.RackID]; ok {
			remaining := target.ComponentsByType.Subtract(excludedComponents)
			if len(remaining) == 0 {
				continue
			}

			target.ComponentsByType = remaining
		}

		result = append(result, target)
	}

	return result
}

// componentsByRackID converts resolved targets into the lookup shape used for
// exclusion by rack ID, merging repeated rack rows instead of overwriting them.
func (t executionTargets) componentsByRackID() (map[uuid.UUID]operation.ComponentsByType, error) {
	if len(t) == 0 {
		return nil, nil
	}

	result := make(map[uuid.UUID]operation.ComponentsByType, len(t))
	for idx, target := range t {
		if existing, ok := result[target.RackID]; ok {
			merged, err := existing.Merge(target.ComponentsByType)
			if err != nil {
				return nil, fmt.Errorf("targets[%d].components_by_type: %w", idx, err)
			}

			result[target.RackID] = merged
		} else {
			normalized, err := target.ComponentsByType.Normalize()
			if err != nil {
				return nil, fmt.Errorf("targets[%d].components_by_type: %w", idx, err)
			}

			result[target.RackID] = normalized
		}
	}

	return result, nil
}

// sortBySeedScore returns a deterministic pseudo-random order based on
// hash(seed:rackID). It clones the slice so callers keep their original order.
func (t executionTargets) sortBySeedScore(seed string) executionTargets {
	ordered := slices.Clone(t)
	slices.SortFunc(ordered, func(a, b operation.RackExecutionTarget) int {
		scoreA := targetScore(seed, a.RackID)
		scoreB := targetScore(seed, b.RackID)
		if cmp := bytes.Compare(scoreA[:], scoreB[:]); cmp != 0 {
			return cmp
		}
		return strings.Compare(a.RackID.String(), b.RackID.String())
	})

	return ordered
}

// targetScore is the stable hash input shared by selector and ordering policies
// that need deterministic seed-based shuffling.
func targetScore(seed string, rackID uuid.UUID) [sha256.Size]byte {
	return sha256.Sum256([]byte(seed + ":" + rackID.String()))
}
