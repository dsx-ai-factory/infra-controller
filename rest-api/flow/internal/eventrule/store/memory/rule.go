// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package memory

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"

	converterdao "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
)

// GetByID returns one persisted rule.
func (s *Store) GetByID(_ context.Context, id uuid.UUID) (*eventrule.Rule, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	persisted, ok := s.rules[id]
	if !ok {
		return nil, fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
	}
	return converterdao.EventRuleFrom(&persisted)
}

// List returns persisted rules matching the filter.
func (s *Store) List(
	_ context.Context,
	request eventrule.RuleListRequest,
) (eventrule.RuleListPage, error) {
	if err := request.Validate(); err != nil {
		return eventrule.RuleListPage{}, err
	}

	s.mu.RLock()
	defer s.mu.RUnlock()

	rules := make([]*eventrule.Rule, 0, len(s.rules))
	for _, persisted := range s.rules {
		rule, err := converterdao.EventRuleFrom(&persisted)
		if err != nil {
			return eventrule.RuleListPage{}, err
		}
		if request.Filter.Matches(rule) {
			rules = append(rules, rule)
		}
	}
	slices.SortFunc(rules, func(a, b *eventrule.Rule) int {
		return cmp.Compare(a.ID.String(), b.ID.String())
	})

	total := len(rules)
	if request.Offset >= total {
		return eventrule.RuleListPage{Total: total}, nil
	}

	end := min(request.Offset+request.Limit, total)
	return eventrule.RuleListPage{
		Rules: rules[request.Offset:end],
		Total: total,
	}, nil
}

// Create stores a new persisted rule.
func (s *Store) Create(
	_ context.Context,
	rule *eventrule.Rule,
) (*eventrule.Rule, error) {
	if rule == nil {
		return nil, errors.New("event rule is nil")
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	if _, ok := s.rules[rule.ID]; ok {
		return nil, fmt.Errorf("event rule %s already exists", rule.ID)
	}
	canonical := rule.Clone()
	now := s.now().UTC()
	canonical.CreatedAt = now
	canonical.UpdatedAt = now
	persisted, err := converterdao.EventRuleTo(&canonical)
	if err != nil {
		return nil, err
	}
	s.rules[canonical.ID] = *persisted
	return &canonical, nil
}

// UpdateMetadata updates one persisted rule's metadata.
func (s *Store) UpdateMetadata(
	_ context.Context,
	id uuid.UUID,
	metadata eventrule.RuleMetadata,
) error {
	return s.updateRule(id, func(rule *eventrule.Rule) {
		rule.Name = metadata.Name
		rule.Description = metadata.Description
	})
}

// ReplaceActions replaces all actions belonging to one persisted rule.
func (s *Store) ReplaceActions(
	_ context.Context,
	id uuid.UUID,
	actions []eventrule.Action,
) error {
	return s.updateRule(id, func(rule *eventrule.Rule) {
		rule.Actions = eventrule.CloneActions(actions)
	})
}

// Delete atomically deletes a persisted rule and all of its bindings.
func (s *Store) Delete(_ context.Context, id uuid.UUID) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, ok := s.rules[id]; !ok {
		return fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
	}
	bindingIDs := make([]uuid.UUID, 0)
	for bindingID, persisted := range s.bindings {
		if persisted.RuleID == id {
			bindingIDs = append(bindingIDs, bindingID)
		}
	}
	delete(s.rules, id)
	for _, bindingID := range bindingIDs {
		delete(s.bindings, bindingID)
	}
	return nil
}

// SetEnabled changes one persisted rule's enabled state.
func (s *Store) SetEnabled(_ context.Context, id uuid.UUID, enabled bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	persisted, ok := s.rules[id]
	if !ok {
		return fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
	}
	if persisted.Enabled == enabled {
		return nil
	}

	rule, err := converterdao.EventRuleFrom(&persisted)
	if err != nil {
		return err
	}
	rule.Enabled = enabled
	rule.UpdatedAt = s.now().UTC()
	updated, err := converterdao.EventRuleTo(rule)
	if err != nil {
		return err
	}
	s.rules[id] = *updated

	return nil
}

func (s *Store) updateRule(id uuid.UUID, mutate func(*eventrule.Rule)) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	persisted, ok := s.rules[id]
	if !ok {
		return fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
	}
	rule, err := converterdao.EventRuleFrom(&persisted)
	if err != nil {
		return err
	}
	mutate(rule)
	rule.UpdatedAt = s.now().UTC()
	updated, err := converterdao.EventRuleTo(rule)
	if err != nil {
		return err
	}
	s.rules[id] = *updated
	return nil
}
