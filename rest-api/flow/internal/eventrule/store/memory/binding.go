// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package memory

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/google/uuid"

	converterdao "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// Bind stores a rule-to-scope association.
func (s *Store) Bind(_ context.Context, binding eventrule.Binding) error {
	if err := binding.Validate(); err != nil {
		return err
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	if _, ok := s.bindings[binding.ID]; ok {
		return fmt.Errorf("event rule binding %s already exists", binding.ID)
	}

	ruleRecord, ok := s.rules[binding.RuleID]
	if !ok {
		return fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, binding.RuleID)
	}

	if string(binding.EventType) != ruleRecord.EventType {
		return fmt.Errorf(
			"event rule binding event type %q does not match rule event type %q",
			binding.EventType,
			ruleRecord.EventType,
		)
	}

	// Keep these checks in separate passes so duplicate-scope conflicts take
	// deterministic precedence over mixed site/rack conflicts regardless of map
	// iteration order.
	for _, persisted := range s.bindings {
		if bindingRecordMatchesScope(persisted, binding.EventType, binding.Scope) {
			return fmt.Errorf(
				"%w: event type %q already has a binding for scope %q",
				eventrule.ErrBindingConflict,
				binding.EventType,
				binding.Scope.Type,
			)
		}
	}

	for _, persisted := range s.bindings {
		if persisted.RuleID == binding.RuleID &&
			persisted.ScopeType != string(binding.Scope.Type) {
			return fmt.Errorf(
				"%w: event rule %s cannot mix site and rack bindings",
				eventrule.ErrBindingConflict,
				binding.RuleID,
			)
		}
	}

	now := s.now().UTC()
	persisted, err := converterdao.EventRuleBindingTo(binding, now, now)
	if err != nil {
		return err
	}
	s.bindings[binding.ID] = *persisted
	return nil
}

// Unbind atomically deletes the binding for an event type and scope.
func (s *Store) Unbind(
	_ context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) error {
	if err := eventType.Validate(); err != nil {
		return err
	}
	if err := scope.Validate(); err != nil {
		return err
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	for id, binding := range s.bindings {
		if bindingRecordMatchesScope(binding, eventType, scope) {
			delete(s.bindings, id)
			return nil
		}
	}

	return fmt.Errorf(
		"%w: binding for event type %q and scope %q",
		eventrule.ErrBindingNotFound,
		eventType,
		scope.Type,
	)
}

// GetForScope returns the binding for an event type and scope.
func (s *Store) GetForScope(
	_ context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	for _, persisted := range s.bindings {
		if bindingRecordMatchesScope(persisted, eventType, scope) {
			return converterdao.EventRuleBindingFrom(&persisted)
		}
	}
	return nil, nil
}

func bindingRecordMatchesScope(
	binding dbmodel.EventRuleBinding,
	eventType eventrule.Type,
	scope eventrule.Scope,
) bool {
	if binding.EventType != string(eventType) ||
		binding.ScopeType != string(scope.Type) {
		return false
	}

	switch scope.Type {
	case eventrule.ScopeTypeSite:
		return binding.ScopeID == nil
	case eventrule.ScopeTypeRack:
		return binding.ScopeID != nil && *binding.ScopeID == scope.ID
	default:
		return false
	}
}

// PutBinding atomically replaces one assignment without changing either policy.
func (s *Store) PutBinding(_ context.Context, binding eventrule.Binding) (*eventrule.BindingPutResult, error) {
	err := binding.Validate()
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	rule, ok := s.rules[binding.RuleID]
	if !ok {
		return nil, eventrule.ErrRuleNotFound
	}
	if rule.EventType != string(binding.EventType) {
		return nil, fmt.Errorf("%w: policy event type does not match binding", eventrule.ErrInvalidRuleInput)
	}
	now := s.now().UTC()
	created := now
	wasCreated := true
	for id, record := range s.bindings {
		if !bindingRecordMatchesScope(record, binding.EventType, binding.Scope) {
			continue
		}
		if record.RuleID == binding.RuleID {
			saved, err := converterdao.EventRuleBindingFrom(&record)
			return &eventrule.BindingPutResult{Binding: saved}, err
		}
		binding.ID = id
		wasCreated = false
		created = record.CreatedAt
		break
	}
	record, err := converterdao.EventRuleBindingTo(binding, created, now)
	if err != nil {
		return nil, err
	}
	s.bindings[binding.ID] = *record
	return &eventrule.BindingPutResult{Binding: &binding, Created: wasCreated}, nil
}

// ListBindings orders explicit references before pagination.
func (s *Store) ListBindings(_ context.Context, request eventrule.BindingListRequest) ([]eventrule.Binding, int, error) {
	err := request.Validate()
	if err != nil {
		return nil, 0, err
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	result := make([]eventrule.Binding, 0)
	for _, record := range s.bindings {
		if request.RuleID != uuid.Nil && record.RuleID != request.RuleID {
			continue
		}
		if request.EventType != "" && record.EventType != string(request.EventType) {
			continue
		}
		if request.ScopeType != "" && record.ScopeType != string(request.ScopeType) {
			continue
		}
		if request.RackID != uuid.Nil && (record.ScopeType != string(eventrule.ScopeTypeRack) || record.ScopeID == nil || *record.ScopeID != request.RackID) {
			continue
		}
		binding, err := converterdao.EventRuleBindingFrom(&record)
		if err != nil {
			return nil, 0, err
		}
		result = append(result, *binding)
	}
	slices.SortFunc(result, func(a, b eventrule.Binding) int {
		if a.Scope.Type != b.Scope.Type {
			if a.Scope.Type == eventrule.ScopeTypeSite {
				return -1
			}
			return 1
		}
		n := cmp.Compare(a.Scope.ID.String(), b.Scope.ID.String())
		if n != 0 {
			return n
		}
		n = cmp.Compare(a.EventType, b.EventType)
		if n != 0 {
			return n
		}
		return cmp.Compare(a.ID.String(), b.ID.String())
	})
	offset, limit := request.Offset, request.Limit
	total := len(result)
	start := min(offset, total)
	return result[start : start+min(limit, total-start)], total, nil
}
