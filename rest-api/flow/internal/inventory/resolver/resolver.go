// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package resolver normalizes inventory resource references into canonical
// Flow inventory objects.
package resolver

import (
	"context"
	"errors"
	"fmt"

	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// ErrUnresolvable identifies an invalid, missing, ambiguous, or malformed
// inventory resource. Retrying the same reference without an inventory change
// cannot resolve it.
var ErrUnresolvable = errors.New("inventory resource cannot be resolved")

// InventoryReader is the lookup capability required by Resolver. Its method
// shapes intentionally align with the underlying inventory manager and store
// implementations so they can satisfy this interface without adapters.
type InventoryReader interface {
	GetComponentByID(context.Context, uuid.UUID) (*component.Component, error)
	GetComponentsByExternalIDs(context.Context, []string) ([]*component.Component, error)
	GetRackByIdentifier(context.Context, identifier.Identifier, bool) (*rack.Rack, error)
}

// Resolver performs canonical inventory identity resolution.
type Resolver struct {
	inventory InventoryReader
}

// New constructs an inventory resource resolver.
func New(inventory InventoryReader) *Resolver {
	return &Resolver{inventory: inventory}
}

// ComponentByID returns the canonical component for one Flow UUID.
func (r *Resolver) ComponentByID(
	ctx context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	if id == uuid.Nil {
		return nil, unresolvableError("component id is required")
	}

	reference := fmt.Sprintf("component id %s", id)
	resolved, err := r.inventory.GetComponentByID(ctx, id)
	if err != nil {
		return nil, classifyLookupError(reference, err)
	}

	resolved, err = validateComponent(resolved, reference)
	if err != nil {
		return nil, err
	}
	if resolved.Info.ID != id {
		return nil, unresolvableError(
			"%s resolved to component id %s",
			reference,
			resolved.Info.ID,
		)
	}

	return resolved, nil
}

// ComponentByExternalID returns the single canonical component matching an
// external ID and optional type. An unknown type requires the ID to resolve
// unambiguously across all component types.
func (r *Resolver) ComponentByExternalID(
	ctx context.Context,
	externalID string,
	componentType devicetypes.ComponentType,
) (*component.Component, error) {
	if externalID == "" {
		return nil, unresolvableError("component external id is required")
	}

	reference := fmt.Sprintf("component external id %q", externalID)
	components, err := r.inventory.GetComponentsByExternalIDs(
		ctx,
		[]string{externalID},
	)
	if err != nil {
		return nil, classifyLookupError(reference, err)
	}

	selected, err := selectComponent(components, componentType)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", reference, err)
	}

	return validateComponent(selected, reference)
}

func selectComponent(
	candidates []*component.Component,
	componentType devicetypes.ComponentType,
) (*component.Component, error) {
	matches := make([]*component.Component, 0, len(candidates))
	for _, candidate := range candidates {
		if candidate == nil {
			continue
		}
		if componentType == devicetypes.ComponentTypeUnknown || candidate.Type == componentType {
			matches = append(matches, candidate)
		}
	}

	if len(matches) == 1 {
		return matches[0], nil
	}

	return nil, unresolvableError("%d matching components", len(matches))
}

// RackByID returns the canonical rack for one Flow UUID. A nil rack, unknown
// rack, or malformed canonical identity returns an error; successful calls
// always return a non-nil rack whose ID matches id.
func (r *Resolver) RackByID(
	ctx context.Context,
	id uuid.UUID,
	withComponents bool,
) (*rack.Rack, error) {
	if id == uuid.Nil {
		return nil, unresolvableError("rack id is required")
	}

	return r.rackByIdentifier(
		ctx,
		identifier.Identifier{ID: id},
		withComponents,
		fmt.Sprintf("rack id %s", id),
	)
}

// RackByExternalID returns the canonical rack for one external inventory ID.
func (r *Resolver) RackByExternalID(
	ctx context.Context,
	externalID string,
	withComponents bool,
) (*rack.Rack, error) {
	if externalID == "" {
		return nil, unresolvableError("rack external id is required")
	}

	return r.rackByIdentifier(
		ctx,
		identifier.Identifier{ExternalID: externalID},
		withComponents,
		fmt.Sprintf("rack external id %q", externalID),
	)
}

// RackByName returns the canonical rack for one inventory name.
func (r *Resolver) RackByName(
	ctx context.Context,
	name string,
	withComponents bool,
) (*rack.Rack, error) {
	if name == "" {
		return nil, unresolvableError("rack name is required")
	}

	return r.rackByIdentifier(
		ctx,
		identifier.Identifier{Name: name},
		withComponents,
		fmt.Sprintf("rack name %q", name),
	)
}

func (r *Resolver) rackByIdentifier(
	ctx context.Context,
	ref identifier.Identifier,
	withComponents bool,
	reference string,
) (*rack.Rack, error) {
	resolved, err := r.inventory.GetRackByIdentifier(
		ctx,
		ref,
		withComponents,
	)
	if err != nil {
		return nil, classifyLookupError(reference, err)
	}

	if resolved == nil || resolved.Info.ID == uuid.Nil {
		return nil, unresolvableError("%s has no canonical id", reference)
	}
	if ref.ID != uuid.Nil && resolved.Info.ID != ref.ID {
		return nil, unresolvableError(
			"%s resolved to rack id %s",
			reference,
			resolved.Info.ID,
		)
	}
	if err := resolved.ValidateComponentIDs(); err != nil {
		return nil, fmt.Errorf(
			"%w: %s has invalid components: %w",
			ErrUnresolvable,
			reference,
			err,
		)
	}

	return resolved, nil
}

func classifyLookupError(reference string, err error) error {
	if status.Code(err) == codes.NotFound {
		return fmt.Errorf("%w: %s: %w", ErrUnresolvable, reference, err)
	}

	return fmt.Errorf("%s: %w", reference, err)
}

func validateComponent(
	resolved *component.Component,
	reference string,
) (*component.Component, error) {
	if resolved == nil || resolved.Info.ID == uuid.Nil {
		return nil, unresolvableError("%s has no canonical id", reference)
	}

	if resolved.Type == devicetypes.ComponentTypeUnknown {
		return nil, unresolvableError("%s has unknown type", reference)
	}

	return resolved, nil
}

func unresolvableError(format string, args ...any) error {
	return fmt.Errorf("%w: %s", ErrUnresolvable, fmt.Sprintf(format, args...))
}
