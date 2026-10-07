// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package processor prepares and processes event-rule envelopes.
package processor

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
)

// enrich resolves the canonical resource information for an event envelope.
func (p *Processor) enrich(
	ctx context.Context,
	envelope eventrule.Envelope,
) (eventrule.ResolvedResource, error) {
	resource := envelope.Resource
	switch resource.Kind {
	case eventrule.ResourceKindRack:
		return p.enrichRackResource(ctx, resource)
	case eventrule.ResourceKindComponent:
		return p.enrichComponentResource(ctx, resource)
	default:
		return eventrule.ResolvedResource{}, terminalError(fmt.Errorf(
			"unsupported resource kind %q",
			resource.Kind,
		))
	}
}

func (p *Processor) enrichRackResource(
	ctx context.Context,
	resource eventrule.Resource,
) (eventrule.ResolvedResource, error) {
	resolved, err := p.resolveRack(ctx, resource)
	if err != nil {
		return eventrule.ResolvedResource{}, classifyInventoryError(err)
	}

	return eventrule.ResolvedResource{
		Kind:   eventrule.ResourceKindRack,
		ID:     resolved.Info.ID,
		RackID: resolved.Info.ID,
	}, nil
}

func (p *Processor) enrichComponentResource(
	ctx context.Context,
	resource eventrule.Resource,
) (eventrule.ResolvedResource, error) {
	resolved, err := p.resolveComponent(ctx, resource)
	if err != nil {
		return eventrule.ResolvedResource{}, classifyInventoryError(err)
	}

	if resolved.RackID == uuid.Nil {
		return eventrule.ResolvedResource{}, terminalError(fmt.Errorf(
			"component %s has no resolved rack",
			resolved.Info.ID,
		))
	}

	resolvedType, err := inventoryresolver.ComponentTypeToFlow(resolved.Type)
	if err != nil {
		return eventrule.ResolvedResource{}, terminalError(fmt.Errorf(
			"component %s type: %w",
			resolved.Info.ID,
			err,
		))
	}

	return eventrule.ResolvedResource{
		Kind:          eventrule.ResourceKindComponent,
		ID:            resolved.Info.ID,
		RackID:        resolved.RackID,
		ComponentType: resolvedType,
	}, nil
}

func (p *Processor) resolveRack(
	ctx context.Context,
	resource eventrule.Resource,
) (*rack.Rack, error) {
	if resource.ID != uuid.Nil {
		return p.inventory.RackByID(ctx, resource.ID, false)
	}

	return p.inventory.RackByExternalID(ctx, resource.ExternalID, false)
}

func (p *Processor) resolveComponent(
	ctx context.Context,
	resource eventrule.Resource,
) (*component.Component, error) {
	if resource.ID != uuid.Nil {
		return p.inventory.ComponentByID(ctx, resource.ID)
	}

	componentType := devicetypes.ComponentTypeUnknown
	if resource.ComponentTypeHint != "" {
		componentType = devicetypes.ComponentTypeFromString(
			string(resource.ComponentTypeHint),
		)
	}

	return p.inventory.ComponentByExternalID(
		ctx,
		resource.ExternalID,
		componentType,
	)
}
