// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package manager owns event-rule management and event processing.
package manager

import (
	"context"
	"errors"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventexecutor "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	eventprocessor "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/processor"
	eventscheduler "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/scheduler"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/google/uuid"
)

// Manager is the event-rule subsystem facade. It owns rule management and the
// internally assembled event-processing runtime.
type Manager struct {
	builtIns       *builtInRegistry
	store          eventrule.Store
	targets        *target.Registry
	executors      *eventexecutor.Registry
	processor      *eventprocessor.Processor
	scheduler      *eventscheduler.Scheduler
	inventory      *inventoryresolver.Resolver
	handlingReader HandlingReader
}

// New constructs a fully assembled event-rule manager.
func New(config Config) (*Manager, error) {
	if err := config.Validate(); err != nil {
		return nil, err
	}

	executors, err := newExecutorRegistry(config)
	if err != nil {
		return nil, err
	}

	targets, err := newTargetRegistry(config)
	if err != nil {
		return nil, err
	}

	builtIns, err := newBuiltInRulesRegistry(executors, targets)
	if err != nil {
		return nil, err
	}

	executionScheduler, err := eventscheduler.New(eventscheduler.Config{
		InstanceID: config.Scheduler.InstanceID,
		Dependencies: eventscheduler.Dependencies{
			Store:     config.Store,
			Executors: executors,
		},
		Runtime: config.Scheduler.Runtime,
		Policy:  config.Scheduler.Policy,
	})
	if err != nil {
		return nil, err
	}

	processor, err := newProcessor(
		config,
		config.Store,
		builtIns,
		targets,
		executionScheduler,
	)
	if err != nil {
		return nil, err
	}

	return &Manager{
		builtIns:       builtIns,
		store:          config.Store,
		targets:        targets,
		executors:      executors,
		processor:      processor,
		scheduler:      executionScheduler,
		inventory:      inventoryresolver.New(config.Inventory),
		handlingReader: config.HandlingReader,
	}, nil
}

func newTargetRegistry(config Config) (*target.Registry, error) {
	targets := target.New()

	err := leakage.RegisterTargetResolvers(
		targets,
		inventoryresolver.New(config.Inventory),
	)
	if err != nil {
		return nil, err
	}

	return targets, nil
}

func newBuiltInRulesRegistry(
	executors *eventexecutor.Registry,
	targets *target.Registry,
) (*builtInRegistry, error) {
	// Register the built-in rule for each supported event type here.
	rules := []*eventrule.Rule{
		leakage.DefaultRule(),
	}

	builtIns := &builtInRegistry{
		byID:        make(map[uuid.UUID]eventrule.Rule, len(rules)),
		byEventType: make(map[eventrule.Type]uuid.UUID, len(rules)),
	}

	for _, rule := range rules {
		if err := builtIns.addRule(rule); err != nil {
			return nil, err
		}
	}

	for _, rule := range rules {
		if err := executors.ValidateActions(rule.Actions); err != nil {
			return nil, fmt.Errorf("validate built-in event rule executors: %w", err)
		}

		if err := targets.ValidateRule(rule); err != nil {
			return nil, fmt.Errorf("validate built-in event rule targets: %w", err)
		}
	}

	return builtIns, nil
}

func newProcessor(
	config Config,
	store eventrule.Store,
	builtIns *builtInRegistry,
	targets *target.Registry,
	notifier eventprocessor.ExecutionNotifier,
) (*eventprocessor.Processor, error) {
	cfg := eventprocessor.Config{
		Inventory: config.Inventory,
		Rules:     &ruleResolver{builtIns: builtIns, store: store},
		Store:     store,
		Targets:   targets,
		Notifier:  notifier,
	}

	return eventprocessor.New(cfg)
}

func newExecutorRegistry(config Config) (*eventexecutor.Registry, error) {
	cfg := eventexecutor.Config{
		TaskManager: config.TaskManager,
		EventTypes:  config.Store,
		AlertSender: config.AlertSender,
	}

	return eventexecutor.New(cfg)
}

// Start launches the internally assembled execution scheduler in the
// background.
func (m *Manager) Start(ctx context.Context) error {
	return m.scheduler.Start(ctx)
}

// Stop stops the execution scheduler and waits for its workers to exit.
func (m *Manager) Stop() error {
	return m.scheduler.Stop()
}

// Process delegates one collected event to the internally assembled processor.
func (m *Manager) Process(ctx context.Context, envelope eventrule.Envelope) error {
	return m.processor.Process(ctx, envelope)
}

// GetByID looks in persisted rules and then built-ins.
func (m *Manager) GetByID(ctx context.Context, id uuid.UUID) (*eventrule.Rule, error) {
	if err := validateRuleID(id); err != nil {
		return nil, err
	}

	rule, err := m.store.GetByID(ctx, id)
	if err == nil {
		return rule, nil
	}

	if !errors.Is(err, eventrule.ErrRuleNotFound) {
		return nil, err
	}

	return m.builtIns.GetByID(ctx, id)
}

// GetEffective returns the enabled rule selected by rack, site, and built-in
// precedence for one concrete rack or component target.
func (m *Manager) GetEffective(
	ctx context.Context,
	eventType eventrule.Type,
	target eventrule.ResourceIdentity,
) (*eventrule.Rule, error) {
	if err := m.validateSupportedEventType(eventType); err != nil {
		return nil, invalidRuleInput(err)
	}

	if err := target.Validate(); err != nil {
		return nil, invalidRuleInput(err)
	}

	rackID, err := m.resolveTargetRackID(ctx, target)
	if err != nil {
		return nil, err
	}

	resolver := &ruleResolver{builtIns: m.builtIns, store: m.store}
	return resolver.GetEffective(ctx, eventType, rackID)
}

func (m *Manager) resolveTargetRackID(
	ctx context.Context,
	target eventrule.ResourceIdentity,
) (uuid.UUID, error) {
	switch target.Kind {
	case eventrule.ResourceKindRack:
		resolved, err := m.inventory.RackByID(ctx, target.ID, false)
		if err != nil {
			return uuid.Nil, effectiveRuleTargetError(err)
		}

		return resolved.Info.ID, nil
	case eventrule.ResourceKindComponent:
		resolved, err := m.inventory.ComponentByID(ctx, target.ID)
		if err != nil {
			return uuid.Nil, effectiveRuleTargetError(err)
		}
		if resolved.RackID == uuid.Nil {
			return uuid.Nil, fmt.Errorf(
				"%w: component %s has no rack",
				eventrule.ErrRuleTargetNotFound,
				target.ID,
			)
		}

		return resolved.RackID, nil
	default:
		return uuid.Nil, invalidRuleInput(fmt.Errorf(
			"unsupported event rule target kind %q",
			target.Kind,
		))
	}
}

func effectiveRuleTargetError(err error) error {
	if errors.Is(err, inventoryresolver.ErrUnresolvable) {
		return fmt.Errorf("%w: %w", eventrule.ErrRuleTargetNotFound, err)
	}

	return err
}

// List returns persisted rules followed by built-ins, with each group ordered
// by ID, and reports the total matching count before pagination.
func (m *Manager) List(
	ctx context.Context,
	request eventrule.RuleListRequest,
) (eventrule.RuleListPage, error) {
	if err := request.Validate(); err != nil {
		return eventrule.RuleListPage{}, invalidRuleInput(err)
	}
	if request.Filter.EventType != nil {
		if err := m.validateSupportedEventType(*request.Filter.EventType); err != nil {
			return eventrule.RuleListPage{}, invalidRuleInput(err)
		}
	}

	persisted, err := m.store.List(ctx, request)
	if err != nil {
		return eventrule.RuleListPage{}, err
	}

	remaining := request.Limit - len(persisted.Rules)
	builtInRequest := request
	builtInRequest.Offset = max(0, request.Offset-persisted.Total)
	// Query at least one built-in so Total is available even when the persisted
	// portion filled the requested page. Built-ins are code-defined and bounded.
	builtInRequest.Limit = max(1, remaining)
	builtIns, err := m.builtIns.List(ctx, builtInRequest)
	if err != nil {
		return eventrule.RuleListPage{}, err
	}

	rules := persisted.Rules
	if remaining > 0 {
		rules = append(rules, builtIns.Rules...)
	}

	return eventrule.RuleListPage{
		Rules: rules,
		Total: persisted.Total + builtIns.Total,
	}, nil
}

// Create validates and atomically stores a policy with its requested initial state.
func (m *Manager) Create(
	ctx context.Context,
	input eventrule.RuleCreate,
) (*eventrule.Rule, error) {
	if err := input.Validate(); err != nil {
		return nil, invalidRuleInput(err)
	}

	rule := eventrule.Rule{
		ID:          uuid.New(),
		Enabled:     input.Enabled,
		Origin:      eventrule.RuleOriginPersisted,
		Name:        input.Metadata.Name,
		Description: input.Metadata.Description,
		EventType:   input.EventType,
		Policy:      input.Policy.Clone(),
	}

	if err := m.validateRuntimeRule(&rule); err != nil {
		return nil, invalidRuleInput(err)
	}

	if err := m.rejectBuiltInID(ctx, rule.ID); err != nil {
		return nil, err
	}

	return m.store.Create(ctx, &rule)
}

// Delete removes an unbound persisted rule. The store atomically rejects
// deletion while any binding references the rule.
func (m *Manager) Delete(ctx context.Context, id uuid.UUID) error {
	err := validateRuleID(id)
	if err != nil {
		return invalidRuleInput(err)
	}

	err = m.rejectBuiltInID(ctx, id)
	if err != nil {
		return err
	}

	return m.store.Delete(ctx, id)
}

// SetEnabled changes whether a persisted rule is enabled.
//
// Enabled-state changes use last-write-wins semantics. Enabling validates the
// immutable rule definition before the store atomically applies the desired
// state. Repeating the current state preserves UpdatedAt.
func (m *Manager) SetEnabled(ctx context.Context, id uuid.UUID, enabled bool) error {
	if err := validateRuleID(id); err != nil {
		return invalidRuleInput(err)
	}

	if err := m.rejectBuiltInID(ctx, id); err != nil {
		return err
	}

	if enabled {
		rule, err := m.store.GetByID(ctx, id)
		if err != nil {
			return err
		}

		if err := m.validateRuntimeRule(rule); err != nil {
			return invalidRuleInput(err)
		}
	}

	return m.store.SetEnabled(ctx, id, enabled)
}

// Bind associates a persisted rule with a scope and returns the new binding.
func (m *Manager) Bind(
	ctx context.Context,
	ruleID uuid.UUID,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	if err := validateRuleID(ruleID); err != nil {
		return nil, invalidRuleInput(err)
	}

	if err := scope.Validate(); err != nil {
		return nil, invalidRuleInput(err)
	}

	if err := m.rejectBuiltInID(ctx, ruleID); err != nil {
		return nil, err
	}

	rule, err := m.store.GetByID(ctx, ruleID)
	if err != nil {
		return nil, err
	}
	if err := m.validateSupportedEventType(rule.EventType); err != nil {
		return nil, invalidRuleInput(err)
	}
	if err := m.validateBindingScope(ctx, scope); err != nil {
		return nil, err
	}

	binding := eventrule.Binding{
		ID:        uuid.New(),
		RuleID:    rule.ID,
		EventType: rule.EventType,
		Scope:     scope,
	}

	if err := binding.Validate(); err != nil {
		return nil, invalidRuleInput(err)
	}

	if err := m.store.Bind(ctx, binding); err != nil {
		return nil, err
	}

	return &binding, nil
}

func (m *Manager) validateBindingScope(ctx context.Context, scope eventrule.Scope) error {
	if scope.Type != eventrule.ScopeTypeRack {
		return nil
	}

	if _, err := m.inventory.RackByID(ctx, scope.ID, false); err != nil {
		if errors.Is(err, inventoryresolver.ErrUnresolvable) {
			return invalidRuleInput(fmt.Errorf(
				"rack scope id %s does not identify an existing rack: %w",
				scope.ID,
				err,
			))
		}

		return fmt.Errorf("resolve rack scope id %s: %w", scope.ID, err)
	}

	return nil
}

// Unbind removes the persisted rule binding occupying an event type and scope.
func (m *Manager) Unbind(
	ctx context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) error {
	if err := m.validateSupportedEventType(eventType); err != nil {
		return invalidRuleInput(err)
	}
	if err := scope.Validate(); err != nil {
		return invalidRuleInput(err)
	}

	return m.store.Unbind(ctx, eventType, scope)
}

// GetBindingForScope returns the persisted binding occupying an event type and
// scope, or nil when that scope has no override.
func (m *Manager) GetBindingForScope(
	ctx context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	if err := m.validateSupportedEventType(eventType); err != nil {
		return nil, invalidRuleInput(err)
	}

	if err := scope.Validate(); err != nil {
		return nil, invalidRuleInput(err)
	}

	return m.store.GetForScope(ctx, eventType, scope)
}

func (m *Manager) rejectBuiltInID(ctx context.Context, id uuid.UUID) error {
	_, err := m.builtIns.GetByID(ctx, id)
	if err == nil {
		return fmt.Errorf(
			"%w: event rule %s is a built-in and cannot be mutated",
			eventrule.ErrBuiltInRuleImmutable,
			id,
		)
	}

	if !errors.Is(err, eventrule.ErrRuleNotFound) {
		return err
	}

	return nil
}

func (m *Manager) validateRuntimeRule(rule *eventrule.Rule) error {
	if rule == nil {
		return fmt.Errorf("event rule is nil")
	}
	if err := m.validateSupportedEventType(rule.EventType); err != nil {
		return err
	}
	if err := m.executors.ValidateActions(rule.Actions); err != nil {
		return err
	}

	return m.targets.ValidateRule(rule)
}

func (m *Manager) validateSupportedEventType(eventType eventrule.Type) error {
	if err := eventType.Validate(); err != nil {
		return err
	}

	if !m.builtIns.supportsEventType(eventType) {
		return fmt.Errorf("unsupported event type %q", eventType)
	}

	return nil
}

func validateRuleID(id uuid.UUID) error {
	if id == uuid.Nil {
		return fmt.Errorf("event rule id is required")
	}

	return nil
}

func invalidRuleInput(err error) error {
	return fmt.Errorf("%w: %w", eventrule.ErrInvalidRuleInput, err)
}

// PutBinding assigns a reusable policy without modifying its definition or state.
func (m *Manager) PutBinding(ctx context.Context, eventType eventrule.Type, id uuid.UUID, scope eventrule.Scope) (*eventrule.BindingPutResult, error) {
	err := m.validateSupportedEventType(eventType)
	if err != nil {
		return nil, invalidRuleInput(err)
	}
	err = scope.Validate()
	if err != nil {
		return nil, invalidRuleInput(err)
	}
	err = m.rejectBuiltInID(ctx, id)
	if err != nil {
		return nil, err
	}
	rule, err := m.GetByID(ctx, id)
	if err != nil {
		return nil, err
	}
	if rule.EventType != eventType {
		return nil, invalidRuleInput(fmt.Errorf("policy event type does not match binding"))
	}
	err = m.validateBindingScope(ctx, scope)
	if err != nil {
		return nil, err
	}
	return m.store.PutBinding(ctx, eventrule.Binding{ID: uuid.New(), RuleID: id, EventType: eventType, Scope: scope})
}

// ListBindings returns explicit policy references, including disabled assignments.
func (m *Manager) ListBindings(ctx context.Context, query eventrule.BindingListRequest) ([]eventrule.Binding, int, error) {
	err := query.Validate()
	if err != nil {
		return nil, 0, invalidRuleInput(err)
	}
	if query.EventType != "" {
		err = m.validateSupportedEventType(query.EventType)
		if err != nil {
			return nil, 0, invalidRuleInput(err)
		}
	}
	return m.store.ListBindings(ctx, query)
}

// ValidateEventType checks a registered type at query boundaries.
func (m *Manager) ValidateEventType(eventType eventrule.Type) error {
	err := m.validateSupportedEventType(eventType)
	if err != nil {
		return invalidRuleInput(err)
	}
	return nil
}

// ListEventHandlings reports operational state using captured associations.
func (m *Manager) ListEventHandlings(ctx context.Context, query eventrule.HandlingQuery) ([]eventrule.Handling, int, error) {
	if query.EventType != "" {
		err := m.ValidateEventType(query.EventType)
		if err != nil {
			return nil, 0, err
		}
	}
	return m.handlingReader.ListEventHandlings(ctx, query)
}
