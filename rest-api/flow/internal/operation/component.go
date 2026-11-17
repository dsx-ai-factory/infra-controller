// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"bytes"
	"encoding/json"
	"fmt"
	"slices"
	"strings"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// -----------------------------------------------------------------------------
// Input component filters
// -----------------------------------------------------------------------------

// The types in this section describe caller-facing component filters. They are
// unresolved expressions that may be persisted with schedule scope rows or
// converted into concrete execution components by a resolver.

// ComponentFilterKind discriminates the two variants of ComponentFilter.
type ComponentFilterKind string

const (
	// ComponentFilterKindTypes filters by component type.
	ComponentFilterKindTypes ComponentFilterKind = "types"
	// ComponentFilterKindComponents targets specific components by UUID.
	ComponentFilterKindComponents ComponentFilterKind = "components"
)

// ComponentFilter is intentionally modeled as a JSON discriminated union rather
// than an interface because callers currently need to validate, persist, and
// compare the filter shape instead of dispatching variant-specific behavior.
// Exactly one of Types or Components must be non-nil when Kind is set.
type ComponentFilter struct {
	Kind ComponentFilterKind `json:"kind"`
	// Types lists the component type strings when Kind == "types".
	Types []string `json:"types,omitempty"`
	// Components lists the component UUIDs when Kind == "components".
	Components []uuid.UUID `json:"components,omitempty"`
}

// Validate checks the discriminated-union invariants of a ComponentFilter.
func (cf *ComponentFilter) Validate() error {
	switch cf.Kind {
	case ComponentFilterKindTypes:
		if len(cf.Types) == 0 {
			return fmt.Errorf(
				"component filter kind %q requires at least one type",
				cf.Kind,
			)
		}
		for _, t := range cf.Types {
			if !devicetypes.IsValidComponentTypeString(t) {
				return fmt.Errorf(
					"component filter kind %q contains unknown type %q",
					cf.Kind, t,
				)
			}
		}
		if len(cf.Components) > 0 {
			return fmt.Errorf(
				"component filter kind %q must not have components set",
				cf.Kind,
			)
		}
	case ComponentFilterKindComponents:
		if len(cf.Components) == 0 {
			return fmt.Errorf(
				"component filter kind %q requires at least one component",
				cf.Kind,
			)
		}
		for idx, id := range cf.Components {
			if id == uuid.Nil {
				return fmt.Errorf(
					"component filter kind %q components[%d] is required",
					cf.Kind,
					idx,
				)
			}
		}
		if len(cf.Types) > 0 {
			return fmt.Errorf(
				"component filter kind %q must not have types set", cf.Kind,
			)
		}
	default:
		return fmt.Errorf("unknown component filter kind: %q", cf.Kind)
	}

	return nil
}

// MarshalComponentFilter marshals a ComponentFilter to JSON for JSONB storage.
func MarshalComponentFilter(cf *ComponentFilter) (json.RawMessage, error) {
	if cf == nil {
		return nil, nil
	}

	if err := cf.Validate(); err != nil {
		return nil, err
	}

	return json.Marshal(cf)
}

// UnmarshalComponentFilter parses target component-filter JSON. Nil, empty,
// and JSON null all mean "no filter", so the target applies to all components
// in the rack.
func UnmarshalComponentFilter(raw json.RawMessage) (*ComponentFilter, error) {
	if isNullFilter(raw) {
		return nil, nil
	}

	var cf ComponentFilter
	if err := json.Unmarshal(raw, &cf); err != nil {
		return nil, err
	}

	if err := cf.Validate(); err != nil {
		return nil, err
	}

	return &cf, nil
}

// ComponentFilterEqual reports whether two component_filter JSONB values are
// semantically equivalent. nil, empty, and the JSON null literal are all
// treated as equivalent (they all mean "all components"). Element order is
// ignored for both type-based and component-UUID filters.
func ComponentFilterEqual(a, b json.RawMessage) (bool, error) {
	aNull, bNull := isNullFilter(a), isNullFilter(b)
	if aNull && bNull {
		return true, nil
	}
	if aNull != bNull {
		return false, nil
	}
	if bytes.Equal(a, b) {
		return true, nil
	}

	cfA, err := UnmarshalComponentFilter(a)
	if err != nil {
		return false, fmt.Errorf("unmarshal component filter: %w", err)
	}
	cfB, err := UnmarshalComponentFilter(b)
	if err != nil {
		return false, fmt.Errorf("unmarshal component filter: %w", err)
	}

	if cfA == nil && cfB == nil {
		return true, nil
	}

	if cfA == nil || cfB == nil {
		return false, nil
	}

	if cfA.Kind != cfB.Kind {
		return false, nil
	}

	switch cfA.Kind {
	case ComponentFilterKindTypes:
		return sliceSetEqual(cfA.Types, cfB.Types), nil
	case ComponentFilterKindComponents:
		return sliceSetEqual(cfA.Components, cfB.Components), nil
	}

	return false, nil
}

func isNullFilter(raw json.RawMessage) bool {
	trimmed := bytes.TrimSpace(raw)
	return len(trimmed) == 0 || string(trimmed) == "null"
}

func sliceSetEqual[T comparable](a, b []T) bool {
	if len(a) != len(b) {
		return false
	}

	counts := make(map[T]int, len(a))
	for _, v := range a {
		counts[v]++
	}

	for _, v := range b {
		counts[v]--
		if counts[v] < 0 {
			return false
		}
	}

	return true
}

// -----------------------------------------------------------------------------
// Execution component sets
// -----------------------------------------------------------------------------

// The types in this section describe resolved component sets. They are used by
// planners, dispatchers, and persisted execution targets after input filters
// have been resolved to concrete component UUIDs.

// ComponentsByType maps each targeted component type to the concrete component
// UUIDs to operate on.
type ComponentsByType map[devicetypes.ComponentType][]uuid.UUID

// Clone returns a deep copy with component UUIDs sorted inside each type.
func (c ComponentsByType) Clone() ComponentsByType {
	if len(c) == 0 {
		return nil
	}

	cloned := make(ComponentsByType, len(c))
	for componentType, ids := range c {
		copied := append([]uuid.UUID(nil), ids...)
		SortComponentUUIDs(copied)
		cloned[componentType] = copied
	}
	return cloned
}

// AllComponentUUIDs returns all component UUIDs across component types in
// deterministic component-type and UUID order.
func (c ComponentsByType) AllComponentUUIDs() []uuid.UUID {
	total := 0
	for _, ids := range c {
		total += len(ids)
	}
	if total == 0 {
		return nil
	}

	result := make([]uuid.UUID, 0, total)
	seen := make(map[uuid.UUID]struct{}, total)
	for _, componentType := range c.SortedComponentTypes() {
		ids := append([]uuid.UUID(nil), c[componentType]...)
		SortComponentUUIDs(ids)
		for _, id := range ids {
			if _, ok := seen[id]; ok {
				continue
			}
			seen[id] = struct{}{}
			result = append(result, id)
		}
	}
	return result
}

// SortedComponentTypes returns component map keys in deterministic order.
func (c ComponentsByType) SortedComponentTypes() []devicetypes.ComponentType {
	componentTypes := make([]devicetypes.ComponentType, 0, len(c))
	for ct := range c {
		componentTypes = append(componentTypes, ct)
	}

	slices.SortFunc(
		componentTypes,
		func(a, b devicetypes.ComponentType) int {
			return strings.Compare(
				devicetypes.ComponentTypeToString(a),
				devicetypes.ComponentTypeToString(b),
			)
		},
	)

	return componentTypes
}

// Validate checks that c is a non-empty, already-normalized component set.
// Duplicate UUIDs within a component type are rejected; call Normalize when
// caller input should be deduplicated first.
func (c ComponentsByType) Validate() error {
	if len(c) == 0 {
		return fmt.Errorf("Non-empty ComponentsByType is required")
	}

	seenComponents := make(map[uuid.UUID]devicetypes.ComponentType)
	for _, ct := range c.SortedComponentTypes() {
		if ct == devicetypes.ComponentTypeUnknown {
			return fmt.Errorf("ComponentsByType contains unknown component type")
		}

		ids := c[ct]
		if len(ids) == 0 {
			return fmt.Errorf(
				"%s must include at least one component",
				devicetypes.ComponentTypeToString(ct),
			)
		}

		for _, id := range ids {
			if id == uuid.Nil {
				return fmt.Errorf(
					"%s contains empty component UUID",
					devicetypes.ComponentTypeToString(ct),
				)
			}

			if existingType, ok := seenComponents[id]; ok {
				if existingType == ct {
					return fmt.Errorf(
						"%s duplicates component %s",
						devicetypes.ComponentTypeToString(ct),
						id,
					)
				}

				return fmt.Errorf(
					"component %s appears under both %s and %s",
					id,
					devicetypes.ComponentTypeToString(existingType),
					devicetypes.ComponentTypeToString(ct),
				)
			}

			seenComponents[id] = ct
		}
	}

	return nil
}

// Normalize returns a copy of c with duplicate UUIDs removed within each
// component type while preserving the first-seen UUID order.
func (c ComponentsByType) Normalize() (ComponentsByType, error) {
	normalized := make(ComponentsByType, len(c))
	for ct, ids := range c {
		unique := make([]uuid.UUID, 0, len(ids))
		seen := make(map[uuid.UUID]struct{}, len(ids))

		for _, id := range ids {
			if _, ok := seen[id]; ok {
				continue
			}
			seen[id] = struct{}{}
			unique = append(unique, id)
		}

		normalized[ct] = unique
	}

	if err := normalized.Validate(); err != nil {
		return nil, err
	}

	return normalized, nil
}

// Merge returns a normalized union of c and n.
func (c ComponentsByType) Merge(n ComponentsByType) (ComponentsByType, error) {
	merged, err := c.Normalize()
	if err != nil {
		return nil, err
	}

	next, err := n.Normalize()
	if err != nil {
		return nil, err
	}

	componentTypesByID := make(map[uuid.UUID]devicetypes.ComponentType)
	for componentType, ids := range merged {
		for _, id := range ids {
			componentTypesByID[id] = componentType
		}
	}

	for componentType, ids := range next {
		for _, id := range ids {
			if existingType, ok := componentTypesByID[id]; ok && existingType != componentType {
				return nil, fmt.Errorf(
					"components_by_type component %s appears under both %s and %s",
					id,
					devicetypes.ComponentTypeToString(existingType),
					devicetypes.ComponentTypeToString(componentType),
				)
			}
			componentTypesByID[id] = componentType
		}
		merged[componentType] = UniqueComponentUUIDs(
			append(merged[componentType], ids...),
		)
	}

	return merged, nil
}

// Subtract returns a copy of c with UUIDs from exclude removed by component
// type. Component-type entries that become empty are omitted.
func (c ComponentsByType) Subtract(exclude ComponentsByType) ComponentsByType {
	if len(c) == 0 {
		return nil
	}

	remaining := make(ComponentsByType, len(c))
	for componentType, ids := range c {
		if len(ids) == 0 {
			continue
		}

		excludedIDs := exclude[componentType]
		if len(excludedIDs) == 0 {
			remaining[componentType] = append([]uuid.UUID(nil), ids...)
			continue
		}

		excluded := make(map[uuid.UUID]struct{}, len(excludedIDs))
		for _, id := range excludedIDs {
			excluded[id] = struct{}{}
		}

		filtered := make([]uuid.UUID, 0, len(ids))
		for _, id := range ids {
			if _, ok := excluded[id]; !ok {
				filtered = append(filtered, id)
			}
		}
		if len(filtered) == 0 {
			continue
		}
		remaining[componentType] = filtered
	}

	if len(remaining) == 0 {
		return nil
	}

	return remaining
}

// UniqueComponentUUIDs sorts ids and removes duplicate UUIDs in place.
func UniqueComponentUUIDs(ids []uuid.UUID) []uuid.UUID {
	if len(ids) == 0 {
		return nil
	}

	SortComponentUUIDs(ids)
	unique := ids[:0]
	var last uuid.UUID
	for idx, id := range ids {
		if idx == 0 || id != last {
			unique = append(unique, id)
			last = id
		}
	}
	return unique
}

// SortComponentUUIDs sorts UUIDs lexicographically for stable JSON and tests.
func SortComponentUUIDs(ids []uuid.UUID) {
	slices.SortFunc(ids, func(a, b uuid.UUID) int {
		return strings.Compare(a.String(), b.String())
	})
}
