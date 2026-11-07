// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"fmt"
	"slices"
	"sync"
)

// KindValidator validates kind-specific rule definitions.
//
// Future child packages can add their own resolver and executor interfaces
// without widening the shared framework.
type KindValidator interface {
	Kind() Kind
	Validate(rule *Rule) error
}

// Registry stores the validators available to one rule engine instance.
type Registry struct {
	mu         sync.RWMutex
	validators map[Kind]KindValidator
}

// NewRegistry returns an empty rule-kind registry.
func NewRegistry() *Registry {
	return &Registry{
		validators: make(map[Kind]KindValidator),
	}
}

// Register adds a kind validator. Register should normally be called during
// startup before rules are evaluated.
func (r *Registry) Register(validator KindValidator) error {
	if r == nil {
		return fmt.Errorf("rule registry is nil")
	}
	if validator == nil {
		return fmt.Errorf("rule kind validator is nil")
	}

	kind := validator.Kind()
	if err := kind.Validate(); err != nil {
		return err
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if r.validators == nil {
		r.validators = make(map[Kind]KindValidator)
	}
	if _, exists := r.validators[kind]; exists {
		return fmt.Errorf("rule kind %q is already registered", kind)
	}

	r.validators[kind] = validator
	return nil
}

// KindValidator returns the registered validator for kind.
func (r *Registry) KindValidator(kind Kind) (KindValidator, bool) {
	if r == nil {
		return nil, false
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	validator, ok := r.validators[kind]
	return validator, ok
}

// Kinds returns the registered kinds in deterministic order.
func (r *Registry) Kinds() []Kind {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	kinds := make([]Kind, 0, len(r.validators))
	for kind := range r.validators {
		kinds = append(kinds, kind)
	}
	r.mu.RUnlock()

	slices.Sort(kinds)

	return kinds
}
