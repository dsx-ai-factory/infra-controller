// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package target

import (
	"fmt"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

type resolverKey struct {
	eventType eventrule.Type
	strategy  eventrule.TargetStrategy
}

// Registry stores generic and event-specific target resolvers.
type Registry struct {
	mu            sync.RWMutex
	generic       map[eventrule.TargetStrategy]Resolver
	eventSpecific map[resolverKey]Resolver
}

// New constructs a registry containing the generic component, rack, and
// affected-components strategies.
func New() *Registry {
	return &Registry{
		generic: map[eventrule.TargetStrategy]Resolver{
			eventrule.TargetStrategyComponent:          componentResolver{},
			eventrule.TargetStrategyRack:               rackResolver{},
			eventrule.TargetStrategyAffectedComponents: affectedComponentsResolver{},
		},
		eventSpecific: make(map[resolverKey]Resolver),
	}
}

// Register associates an event-specific strategy with a Resolver
// implementation. An event-specific resolver takes precedence over the generic
// strategy resolver.
func (r *Registry) Register(
	eventType eventrule.Type,
	strategy eventrule.TargetStrategy,
	resolver Resolver,
) error {
	if r == nil {
		return fmt.Errorf("target resolver registry is nil")
	}
	if err := eventType.Validate(); err != nil {
		return err
	}
	if err := strategy.Validate(); err != nil {
		return err
	}
	if !strategy.RequiresResolution() {
		return fmt.Errorf("target strategy %q does not require resolution", strategy)
	}
	// Check the interface itself only. Registrations are controlled by this
	// service, so a typed-nil implementation is a programming bug and does not
	// justify reflection-based validation here.
	if resolver == nil {
		return fmt.Errorf("target resolver is required")
	}
	key := resolverKey{eventType: eventType, strategy: strategy}
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, exists := r.eventSpecific[key]; exists {
		return fmt.Errorf(
			"target resolver already registered for event type %q and strategy %q",
			eventType,
			strategy,
		)
	}
	r.eventSpecific[key] = resolver
	return nil
}

// Lookup returns the event-specific resolver when registered, otherwise the
// generic resolver for the strategy. It returns either a non-nil resolver and
// a nil error, or a nil resolver and a non-nil error; it never returns
// (nil, nil).
func (r *Registry) Lookup(
	eventType eventrule.Type,
	strategy eventrule.TargetStrategy,
) (Resolver, error) {
	if r == nil {
		return nil, fmt.Errorf("%w: target resolver registry is nil", ErrUnresolvable)
	}
	if err := eventType.Validate(); err != nil {
		return nil, fmt.Errorf("%w: event type: %v", ErrUnresolvable, err)
	}
	if err := strategy.Validate(); err != nil {
		return nil, fmt.Errorf("%w: target strategy: %v", ErrUnresolvable, err)
	}

	resolver := r.lookup(eventType, strategy)
	if resolver == nil {
		return nil, fmt.Errorf(
			"%w: "+
				"no target resolver for event type %q and strategy %q",
			ErrUnresolvable,
			eventType,
			strategy,
		)
	}

	return resolver, nil
}

// ValidateRule checks that every task action in a valid rule has a registered
// target resolver.
func (r *Registry) ValidateRule(rule *eventrule.Rule) error {
	if r == nil {
		return fmt.Errorf("target resolver registry is nil")
	}

	if err := rule.Validate(); err != nil {
		return err
	}

	for _, action := range rule.Actions {
		strategy := action.Spec.TargetResolutionStrategy()
		if !strategy.RequiresResolution() {
			continue
		}
		if _, err := r.Lookup(rule.EventType, strategy); err != nil {
			return fmt.Errorf(
				"event rule %s action %q has no target resolver for strategy %q",
				rule.ID,
				action.Name,
				strategy,
			)
		}
	}

	return nil
}

func (r *Registry) lookup(
	eventType eventrule.Type,
	strategy eventrule.TargetStrategy,
) Resolver {
	key := resolverKey{eventType: eventType, strategy: strategy}

	r.mu.RLock()
	defer r.mu.RUnlock()

	if resolver := r.eventSpecific[key]; resolver != nil {
		return resolver
	}

	return r.generic[strategy]
}
