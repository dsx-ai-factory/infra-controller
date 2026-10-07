// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func (p *Processor) plan(
	ctx context.Context,
	prepared *preparedEvent,
) (*eventrule.Event, error) {
	event := &prepared.Event
	planned := make([]eventrule.PlannedExecution, len(event.EffectivePolicy.Actions))
	for i, action := range event.EffectivePolicy.Actions {
		executionPlan, err := p.planAction(ctx, *event, prepared.Resource, action)
		if err != nil {
			return nil, fmt.Errorf("plan action %q: %w", action.Name, err)
		}

		planned[i] = eventrule.PlannedExecution{
			ActionName:    action.Name,
			ExecutionPlan: executionPlan,
		}
	}

	committed, err := p.store.CommitEventPlan(ctx, *event, planned)
	if err != nil {
		return nil, fmt.Errorf("persist event plan: %w", err)
	}

	return committed, nil
}

func (p *Processor) planAction(
	ctx context.Context,
	event eventrule.Event,
	resource eventrule.ResolvedResource,
	action eventrule.Action,
) (eventrule.ExecutionPlan, error) {
	switch spec := action.Spec.(type) {
	case *eventrule.SubmitTask:
		return p.planSubmitTask(ctx, event, resource, spec)
	case *eventrule.SendAlert:
		return &eventrule.SendAlertPlan{Severity: spec.Severity, Message: spec.Message}, nil
	case *eventrule.Noop:
		return &eventrule.NoopPlan{Reason: spec.Reason}, nil
	default:
		return nil, fmt.Errorf("unsupported action spec %T", action.Spec)
	}
}

func (p *Processor) planSubmitTask(
	ctx context.Context,
	event eventrule.Event,
	resource eventrule.ResolvedResource,
	spec *eventrule.SubmitTask,
) (eventrule.ExecutionPlan, error) {
	resolved, err := p.resolveTargetRequest(ctx, target.ResolveRequest{
		EventType: event.Type,
		Resource:  resource,
		Strategy:  spec.TargetStrategy,
	})
	if err != nil {
		if isTerminalTargetError(err) {
			return nil, terminalError(err)
		}
		return nil, err
	}

	targets, err := p.materializeTaskTargets(ctx, resolved)
	if err != nil {
		return nil, err
	}

	info, err := spec.Operation.Marshal()
	if err != nil {
		return nil, fmt.Errorf("marshal task operation: %w", err)
	}

	description := spec.Description
	if description == "" {
		description = spec.Operation.Description()
	}

	conflictStrategy := operation.ConflictStrategyReject
	if spec.ConflictStrategy == eventrule.ConflictStrategyQueue {
		conflictStrategy = operation.ConflictStrategyQueue
	}

	return &eventrule.SubmitTaskPlan{
		Operation: operation.Wrapper{
			Type: spec.Operation.Type(),
			Code: spec.Operation.CodeString(),
			Info: info,
		},
		Description:      description,
		ConflictStrategy: conflictStrategy,
		Targets:          targets,
	}, nil
}

func (p *Processor) resolveTargetRequest(
	ctx context.Context,
	request target.ResolveRequest,
) ([]target.Target, error) {
	resolver, err := p.targets.Lookup(request.EventType, request.Strategy)
	if err != nil {
		return nil, err
	}

	resolved, err := resolver.Resolve(ctx, request)
	if err != nil {
		return nil, err
	}

	for i, candidate := range resolved {
		if err := candidate.Validate(); err != nil {
			return nil, fmt.Errorf("%w: resolver target %d: %v", target.ErrUnresolvable, i, err)
		}
	}

	return resolved, nil
}

func (p *Processor) materializeTaskTargets(
	ctx context.Context,
	resolved []target.Target,
) ([]operation.RackExecutionTarget, error) {
	byRack := make(map[uuid.UUID]operation.ComponentsByType)
	for _, candidate := range resolved {
		components, err := p.componentsForTarget(ctx, candidate)
		if err != nil {
			return nil, err
		}

		if len(components) == 0 {
			continue
		}

		if existing := byRack[candidate.RackID]; len(existing) > 0 {
			components, err = existing.Merge(components)
			if err != nil {
				return nil, fmt.Errorf("merge rack %s components: %w", candidate.RackID, err)
			}
		}

		byRack[candidate.RackID] = components
	}

	targets := make([]operation.RackExecutionTarget, 0, len(byRack))
	for rackID, components := range byRack {
		targets = append(targets, operation.RackExecutionTarget{
			RackID:           rackID,
			ComponentsByType: components.Clone(),
		})
	}

	slices.SortFunc(targets, func(a, b operation.RackExecutionTarget) int {
		return cmp.Compare(a.RackID.String(), b.RackID.String())
	})

	return targets, nil
}

func (p *Processor) componentsForTarget(
	ctx context.Context,
	candidate target.Target,
) (operation.ComponentsByType, error) {
	switch candidate.Kind {
	case eventrule.ResourceKindComponent:
		component, err := p.inventory.ComponentByID(ctx, candidate.ID)
		if err != nil {
			return nil, classifyInventoryError(err)
		}

		if component.RackID != candidate.RackID {
			return nil, terminalError(fmt.Errorf(
				"component %s belongs to rack %s, resolver selected rack %s",
				candidate.ID,
				component.RackID,
				candidate.RackID,
			))
		}

		return operation.ComponentsByType{component.Type: []uuid.UUID{component.Info.ID}}, nil
	case eventrule.ResourceKindRack:
		rack, err := p.inventory.RackByID(ctx, candidate.RackID, true)
		if err != nil {
			return nil, classifyInventoryError(err)
		}

		components := make(operation.ComponentsByType)
		for _, component := range rack.Components {
			if component.Type == devicetypes.ComponentTypeUnknown {
				return nil, terminalError(fmt.Errorf(
					"rack %s component %s has unknown type",
					rack.Info.ID,
					component.Info.ID,
				))
			}

			components[component.Type] = append(components[component.Type], component.Info.ID)
		}

		if len(components) == 0 {
			return nil, nil
		}

		return components.Normalize()
	default:
		return nil, terminalError(fmt.Errorf("unsupported target kind %q", candidate.Kind))
	}
}
