// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// TargetFetcher provides the methods needed to fetch racks and components for target resolution.
type TargetFetcher interface {
	GetRackByIdentifier(ctx context.Context, identifier identifier.Identifier, withComponents bool) (*rack.Rack, error)
	GetRacksForNVLDomain(ctx context.Context, domainIdentifier identifier.Identifier) ([]*rack.Rack, error)
	GetComponentByID(ctx context.Context, id uuid.UUID) (*component.Component, error)
	GetComponentByBMCMAC(ctx context.Context, macAddress string) (*component.Component, error)
	GetComponentsByExternalIDs(ctx context.Context, externalIDs []string) ([]*component.Component, error)
}

// resolveTargetSpecToRacks resolves a TargetSpec to components and groups them by rack.
// Returns a map of rackID -> *rack.Rack (with selected components).
func resolveTargetSpecToRacks(
	ctx context.Context,
	fetcher TargetFetcher,
	targetSpec *operation.TargetSpec,
) (map[uuid.UUID]*rack.Rack, error) {
	if err := targetSpec.Validate(); err != nil {
		return nil, fmt.Errorf("invalid target spec: %w", err)
	}

	if targetSpec.IsRackTargeting() {
		return resolveRackTargetSpec(ctx, fetcher, targetSpec.Racks)
	}

	if targetSpec.IsNVLDomainTargeting() {
		return resolveNVLDomainTargetSpec(ctx, fetcher, targetSpec.NVLDomains)
	}

	if targetSpec.IsComponentTargeting() {
		return resolveComponentTargetSpec(ctx, fetcher, targetSpec.Components)
	}

	// This should be detected by Validate() and should never be here, but
	// just in case, handle it anyway.
	return nil, fmt.Errorf("target spec must have one of racks, NVLink domains, or components set")
}

func resolveNVLDomainTargetSpec(
	ctx context.Context,
	fetcher TargetFetcher,
	targets []operation.NVLDomainTarget,
) (map[uuid.UUID]*rack.Rack, error) {
	rackTargets, err := inventoryresolver.ResolveNVLDomainRackTargets(
		ctx,
		fetcher,
		targets,
	)
	if err != nil {
		return nil, fmt.Errorf("failed to resolve NVLink domain targets: %w", err)
	}

	return resolveRackTargetSpec(ctx, fetcher, rackTargets)
}

func resolveRackTargetSpec(
	ctx context.Context,
	fetcher TargetFetcher,
	targets []operation.RackTarget,
) (map[uuid.UUID]*rack.Rack, error) {
	rackMap := make(map[uuid.UUID]*rack.Rack)
	for _, rt := range targets {
		resolved, err := resolveRackTarget(ctx, fetcher, &rt)
		if err != nil {
			return nil, fmt.Errorf("failed to resolve rack target: %w", err)
		}

		if cur, ok := rackMap[resolved.Info.ID]; !ok {
			rackMap[resolved.Info.ID] = resolved
		} else {
			// Merge the components into the existing rack.
			for _, comp := range resolved.Components {
				cur.AddComponent(comp)
			}
		}
	}

	return rackMap, nil
}

func resolveRackTarget(
	ctx context.Context,
	fetcher TargetFetcher,
	rt *operation.RackTarget,
) (*rack.Rack, error) {
	rackObj, err := fetcher.GetRackByIdentifier(ctx, rt.Identifier, true)
	if err != nil {
		return nil, fmt.Errorf("failed to get rack by identifier %+v: %w", rt.Identifier, err)
	}

	if rackObj == nil {
		return nil, fmt.Errorf("rack not found for identifier %+v", rt.Identifier)
	}

	var components []component.Component

	if len(rt.ComponentTypes) > 0 {
		// Filter by component type.
		requiredTypes := make(map[devicetypes.ComponentType]bool)
		for _, ctype := range rt.ComponentTypes {
			requiredTypes[ctype] = true
		}
		for _, comp := range rackObj.Components {
			if requiredTypes[comp.Type] {
				components = append(components, comp)
			}
		}
	} else {
		// All components.
		components = rackObj.Components
	}

	// Do the following so that the internal map for components can be built up
	// correctly so that new components can be merged in correctly.
	r := rack.New(rackObj.Info, rackObj.Loc)
	for _, comp := range components {
		r.AddComponent(comp)
	}

	return r, nil
}

func resolveComponentTargetSpec(
	ctx context.Context,
	fetcher TargetFetcher,
	components []operation.ComponentTarget,
) (map[uuid.UUID]*rack.Rack, error) {
	componentObjs := make([]*component.Component, 0)
	for _, ct := range components {
		comp, err := fetchComponentTarget(ctx, fetcher, &ct)
		if err != nil {
			return nil, fmt.Errorf("failed to get component target %s: %w", ct.TargetIdentifier(), err)
		}
		componentObjs = append(componentObjs, comp)
	}

	rackMap := make(map[uuid.UUID]*rack.Rack)
	for _, co := range componentObjs {
		if cur := rackMap[co.RackID]; cur == nil {
			rackObj, err := fetcher.GetRackByIdentifier(
				ctx,
				identifier.Identifier{ID: co.RackID},
				false,
			)
			if err != nil {
				return nil, fmt.Errorf("failed to get rack by id %s: %w", co.RackID, err)
			}

			rackMap[co.RackID] = rack.New(rackObj.Info, rackObj.Loc)
		}

		rackMap[co.RackID].AddComponent(*co)
	}

	return rackMap, nil
}

func fetchComponentTarget(
	ctx context.Context,
	fetcher TargetFetcher,
	ct *operation.ComponentTarget,
) (*component.Component, error) {
	if ct.UUID != uuid.Nil {
		return fetcher.GetComponentByID(ctx, ct.UUID)
	}

	if ct.External != nil {
		return inventoryresolver.ResolveComponentIdentifier(
			ctx,
			fetcher,
			ct.External.ID,
			ct.External.Type,
		)
	}

	return nil, fmt.Errorf("invalid component target: %+v", ct)
}
