// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package planner

import (
	"context"
	"fmt"
	"net"
	"slices"

	"github.com/google/uuid"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	inventorycomponent "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// InventoryTargetSource resolves inventory objects used by operation-run
// planning. inventory.Manager and inventory/store.PostgresStore both satisfy
// this interface.
type InventoryTargetSource interface {
	GetRackByIdentifier(ctx context.Context, identifier identifier.Identifier, withComponents bool) (*rack.Rack, error)
	GetRacksForNVLDomain(ctx context.Context, domainIdentifier identifier.Identifier) ([]*rack.Rack, error)
	GetListOfRacks(ctx context.Context, info dbquery.StringQueryInfo, manufacturerFilter *dbquery.StringQueryInfo, modelFilter *dbquery.StringQueryInfo, pagination *dbquery.Pagination, orderBy *dbquery.OrderBy, withComponents bool) ([]*rack.Rack, int32, error)
	GetComponentByID(ctx context.Context, id uuid.UUID) (*inventorycomponent.Component, error)
	GetComponentByBMCMAC(ctx context.Context, macAddress string) (*inventorycomponent.Component, error)
	GetComponentsByExternalIDs(ctx context.Context, externalIDs []string) ([]*inventorycomponent.Component, error)
}

// OperationRunTargetSource resolves materialized operation-run targets used as
// exclusion sources while planning a new run.
type OperationRunTargetSource interface {
	ListTargets(ctx context.Context, id uuid.UUID, opts operationrun.TargetListOptions) ([]*operationrun.OperationRunTarget, int32, error)
}

// InventoryTargetLookup resolves planner target scopes from inventory and
// already-materialized operation-run target rows.
type InventoryTargetLookup struct {
	inventory    InventoryTargetSource
	operationRun OperationRunTargetSource
}

var _ TargetLookup = (*InventoryTargetLookup)(nil)

// NewInventoryTargetLookup creates a TargetLookup backed by inventory and
// operation-run target storage.
func NewInventoryTargetLookup(
	inventory InventoryTargetSource,
	operationRun OperationRunTargetSource,
) *InventoryTargetLookup {
	return &InventoryTargetLookup{
		inventory:    inventory,
		operationRun: operationRun,
	}
}

// TargetsFromDefaultScope returns all rack execution targets in inventory,
// optionally narrowed by the operation's default-scope component filter.
func (l *InventoryTargetLookup) TargetsFromDefaultScope(
	ctx context.Context,
	op *operationrun.Operation,
	opts TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	if err := l.requireInventory(); err != nil {
		return nil, err
	}
	if op == nil {
		return nil, fmt.Errorf("operation is required")
	}

	targets := make([]operation.RackExecutionTarget, 0)
	pageSize := targetLookupPageSize(opts)
	for offset := 0; ; {
		racks, total, err := l.inventory.GetListOfRacks(
			ctx,
			dbquery.StringQueryInfo{},
			nil,
			nil,
			&dbquery.Pagination{
				Offset: offset,
				Limit:  pageSize,
			},
			nil,
			true,
		)
		if err != nil {
			return nil, err
		}

		for _, r := range racks {
			target, ok := executionTargetFromRack(
				r,
				op.TargetScope.DefaultScopeComponentFilter,
			)
			if !ok {
				continue
			}

			targets = append(targets, target)
			if err := checkTargetCount(len(targets), opts); err != nil {
				return nil, err
			}
		}

		if len(racks) == 0 || offset+len(racks) >= int(total) {
			break
		}

		offset += len(racks)
	}

	return targets, nil
}

// TargetsFromSpec resolves an explicit operation target spec to rack execution
// targets.
func (l *InventoryTargetLookup) TargetsFromSpec(
	ctx context.Context,
	spec *operation.TargetSpec,
	opts TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	if err := l.requireInventory(); err != nil {
		return nil, err
	}
	if err := spec.Validate(); err != nil {
		return nil, err
	}

	var targets []operation.RackExecutionTarget
	var err error
	if spec.IsRackTargeting() {
		targets, err = l.targetsFromRackSpec(ctx, spec.Racks)
	} else if spec.IsNVLDomainTargeting() {
		targets, err = l.targetsFromNVLDomainSpec(ctx, spec.NVLDomains)
	} else {
		targets, err = l.targetsFromComponentSpec(ctx, spec.Components)
	}
	if err != nil {
		return nil, err
	}

	return targets, checkTargetCount(len(targets), opts)
}

// TargetsFromRuns resolves materialized targets from previous operation runs.
func (l *InventoryTargetLookup) TargetsFromRuns(
	ctx context.Context,
	runIDs []uuid.UUID,
	opts TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	if l == nil || l.operationRun == nil {
		return nil, fmt.Errorf("operation run target source is required")
	}

	targets := make([]operation.RackExecutionTarget, 0)
	for _, runID := range runIDs {
		if runID == uuid.Nil {
			return nil, fmt.Errorf("operation run ID is required")
		}

		runTargets, _, err := l.operationRun.ListTargets(
			ctx,
			runID,
			operationrun.TargetListOptions{
				PhaseScope: operationrun.TargetPhaseScopeAllMaterializedTargets,
			},
		)
		if err != nil {
			return nil, err
		}

		for _, target := range runTargets {
			if target == nil {
				continue
			}
			targets = append(
				targets,
				operation.RackExecutionTarget{
					RackID:           target.RackID,
					ComponentsByType: target.ComponentsByType.Clone(),
				},
			)
		}
		if err := checkTargetCount(len(targets), opts); err != nil {
			return nil, err
		}
	}

	return targets, nil
}

func (l *InventoryTargetLookup) requireInventory() error {
	if l == nil || l.inventory == nil {
		return fmt.Errorf("inventory target source is required")
	}

	return nil
}

func (l *InventoryTargetLookup) targetsFromNVLDomainSpec(
	ctx context.Context,
	domainTargets []operation.NVLDomainTarget,
) ([]operation.RackExecutionTarget, error) {
	rackTargets, err := inventoryresolver.ResolveNVLDomainRackTargets(
		ctx,
		l.inventory,
		domainTargets,
	)
	if err != nil {
		return nil, err
	}

	targets := make([]operation.RackExecutionTarget, 0)
	for rackIndex, rackTarget := range rackTargets {
		r, err := l.inventory.GetRackByIdentifier(
			ctx,
			rackTarget.Identifier,
			true,
		)
		if err != nil {
			return nil, fmt.Errorf("NVLink domain rack target %d: %w", rackIndex, err)
		}

		target, ok := executionTargetFromRack(
			r,
			componentFilterFromTypes(rackTarget.ComponentTypes),
		)
		if ok {
			targets = append(targets, target)
		}
	}

	normalized, err := executionTargets(targets).normalize()
	if err != nil {
		return nil, fmt.Errorf("normalize NVLink domain targets: %w", err)
	}

	return []operation.RackExecutionTarget(normalized), nil
}

func (l *InventoryTargetLookup) targetsFromRackSpec(
	ctx context.Context,
	rackTargets []operation.RackTarget,
) ([]operation.RackExecutionTarget, error) {
	targets := make([]operation.RackExecutionTarget, 0, len(rackTargets))
	for idx, rackTarget := range rackTargets {
		r, err := l.inventory.GetRackByIdentifier(ctx, rackTarget.Identifier, true)
		if err != nil {
			return nil, fmt.Errorf("rack target %d: %w", idx, err)
		}

		target, ok := executionTargetFromRack(
			r,
			componentFilterFromTypes(rackTarget.ComponentTypes),
		)
		if ok {
			targets = append(targets, target)
		}
	}

	return targets, nil
}

func (l *InventoryTargetLookup) targetsFromComponentSpec(
	ctx context.Context,
	componentTargets []operation.ComponentTarget,
) ([]operation.RackExecutionTarget, error) {
	targets := make([]operation.RackExecutionTarget, 0, len(componentTargets))
	externalComponents, err := l.externalComponents(ctx, componentTargets)
	if err != nil {
		return nil, err
	}

	for idx, componentTarget := range componentTargets {
		comp, err := l.componentFromTarget(ctx, componentTarget, externalComponents)
		if err != nil {
			return nil, fmt.Errorf("component target %d: %w", idx, err)
		}

		if comp.RackID == uuid.Nil {
			return nil, fmt.Errorf(
				"component target %d component %s is not assigned to a rack",
				idx,
				comp.Info.ID,
			)
		}

		targets = append(targets, executionTargetFromComponent(comp))
	}

	return targets, nil
}

func (l *InventoryTargetLookup) externalComponents(
	ctx context.Context,
	componentTargets []operation.ComponentTarget,
) ([]*inventorycomponent.Component, error) {
	externalIDs := make([]string, 0, len(componentTargets))
	for _, componentTarget := range componentTargets {
		if componentTarget.External != nil {
			externalIDs = append(externalIDs, componentTarget.External.ID)
		}
	}
	if len(externalIDs) == 0 {
		return nil, nil
	}

	return l.inventory.GetComponentsByExternalIDs(ctx, externalIDs)
}

func (l *InventoryTargetLookup) componentFromTarget(
	ctx context.Context,
	target operation.ComponentTarget,
	externalComponents []*inventorycomponent.Component,
) (*inventorycomponent.Component, error) {
	if target.UUID != uuid.Nil {
		return l.inventory.GetComponentByID(ctx, target.UUID)
	}
	if target.External == nil {
		return nil, fmt.Errorf("component target is required")
	}
	if _, err := net.ParseMAC(target.External.ID); err == nil {
		return inventoryresolver.ResolveComponentIdentifier(
			ctx,
			l.inventory,
			target.External.ID,
			target.External.Type,
		)
	}

	var matched *inventorycomponent.Component
	for _, comp := range externalComponents {
		if comp == nil || comp.ComponentID != target.External.ID {
			continue
		}
		if target.External.Type != devicetypes.ComponentTypeUnknown && comp.Type != target.External.Type {
			continue
		}
		if matched != nil {
			return nil, status.Errorf(
				codes.FailedPrecondition,
				"component %s is ambiguous",
				target.TargetIdentifier(),
			)
		}
		matched = comp
	}
	if matched != nil {
		return matched, nil
	}

	return nil, status.Errorf(
		codes.NotFound,
		"component %s not found",
		target.TargetIdentifier(),
	)
}

func executionTargetFromRack(
	r *rack.Rack,
	filter *operation.ComponentFilter,
) (operation.RackExecutionTarget, bool) {
	if r == nil {
		return operation.RackExecutionTarget{}, false
	}

	components := componentsByTypeFromRack(r, filter)
	if len(components) == 0 {
		return operation.RackExecutionTarget{}, false
	}

	return operation.RackExecutionTarget{
		RackID:           r.Info.ID,
		ComponentsByType: components,
	}, true
}

func executionTargetFromComponent(
	comp *inventorycomponent.Component,
) operation.RackExecutionTarget {
	return operation.RackExecutionTarget{
		RackID: comp.RackID,
		ComponentsByType: operation.ComponentsByType{
			comp.Type: {comp.Info.ID},
		},
	}
}

func componentsByTypeFromRack(
	r *rack.Rack,
	filter *operation.ComponentFilter,
) operation.ComponentsByType {
	componentsByType := make(operation.ComponentsByType)
	for _, comp := range r.Components {
		if !componentMatchesFilter(comp, filter) {
			continue
		}
		componentsByType[comp.Type] = append(
			componentsByType[comp.Type],
			comp.Info.ID,
		)
	}

	if len(componentsByType) == 0 {
		return nil
	}

	return componentsByType
}

func componentMatchesFilter(
	comp inventorycomponent.Component,
	filter *operation.ComponentFilter,
) bool {
	if filter == nil {
		return true
	}

	switch filter.Kind {
	case operation.ComponentFilterKindTypes:
		return slices.ContainsFunc(
			filter.Types,
			func(componentType string) bool {
				return devicetypes.ComponentTypeFromString(componentType) == comp.Type
			},
		)
	case operation.ComponentFilterKindComponents:
		return slices.Contains(filter.Components, comp.Info.ID)
	default:
		return false
	}
}

func componentFilterFromTypes(
	componentTypes []devicetypes.ComponentType,
) *operation.ComponentFilter {
	if len(componentTypes) == 0 {
		return nil
	}

	types := make([]string, 0, len(componentTypes))
	for _, componentType := range componentTypes {
		types = append(types, devicetypes.ComponentTypeToString(componentType))
	}

	return &operation.ComponentFilter{
		Kind:  operation.ComponentFilterKindTypes,
		Types: types,
	}
}

func targetLookupPageSize(opts TargetLookupOptions) int {
	if opts.MaxTargets <= 0 {
		return dbquery.DefaultPaginationLimit
	}

	return opts.MaxTargets + 1
}

func checkTargetCount(
	count int,
	opts TargetLookupOptions,
) error {
	if opts.MaxTargets > 0 && count > opts.MaxTargets {
		return fmt.Errorf(
			"operation run candidate scope exceeds target limit %d",
			opts.MaxTargets,
		)
	}

	return nil
}
