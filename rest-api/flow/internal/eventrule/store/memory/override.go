// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package memory

import (
	"cmp"
	"context"
	"slices"

	converterdao "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// ListRackOverrides follows the persistent store's ordering for missing public
// Rack IDs: the in-memory store does not contain inventory records.
func (s *Store) ListRackOverrides(_ context.Context, eventType eventrule.Type, offset, limit int) ([]eventrule.ScopedPolicy, int, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	bindings := make([]dbmodel.EventRuleBinding, 0)
	for _, binding := range s.bindings {
		if binding.EventType == string(eventType) && binding.ScopeType == string(eventrule.ScopeTypeRack) && s.rules[binding.RuleID].Enabled {
			bindings = append(bindings, binding)
		}
	}
	slices.SortFunc(bindings, func(a, b dbmodel.EventRuleBinding) int { return cmp.Compare(a.ScopeID.String(), b.ScopeID.String()) })
	total := len(bindings)
	policies := make([]eventrule.ScopedPolicy, 0)
	if limit == 0 || offset >= total {
		return policies, total, nil
	}
	for _, binding := range bindings[offset:min(offset+limit, total)] {
		record := s.rules[binding.RuleID]
		rule, err := converterdao.EventRuleFrom(&record)
		if err != nil {
			return nil, 0, err
		}
		policies = append(policies, eventrule.ScopedPolicy{Rule: rule, Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: *binding.ScopeID}})
	}
	return policies, total, nil
}

func (s *Store) SetRuleOverride(_ context.Context, rule *eventrule.Rule, binding eventrule.Binding) (*eventrule.Rule, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, persisted := range s.bindings {
		if !bindingRecordMatchesScope(persisted, binding.EventType, binding.Scope) {
			continue
		}
		canonical := rule.Clone()
		shared := false
		for otherID, other := range s.bindings {
			if otherID != id && other.RuleID == persisted.RuleID {
				shared = true
				break
			}
		}
		if shared {
			now := s.now().UTC()
			canonical.CreatedAt, canonical.UpdatedAt = now, now
			record, err := converterdao.EventRuleTo(&canonical)
			if err != nil {
				return nil, err
			}
			persisted.RuleID = canonical.ID
			persisted.UpdatedAt = now
			s.rules[canonical.ID] = *record
			s.bindings[id] = persisted
			return &canonical, nil
		}
		canonical.ID = persisted.RuleID
		canonical.CreatedAt = s.rules[persisted.RuleID].CreatedAt
		canonical.UpdatedAt = s.now().UTC()
		record, err := converterdao.EventRuleTo(&canonical)
		if err != nil {
			return nil, err
		}
		s.rules[canonical.ID] = *record
		s.bindings[id] = persisted
		return &canonical, nil
	}
	canonical := rule.Clone()
	now := s.now().UTC()
	canonical.CreatedAt, canonical.UpdatedAt = now, now
	record, err := converterdao.EventRuleTo(&canonical)
	if err != nil {
		return nil, err
	}
	bindingRecord, err := converterdao.EventRuleBindingTo(binding, now, now)
	if err != nil {
		return nil, err
	}
	s.rules[canonical.ID] = *record
	s.bindings[binding.ID] = *bindingRecord
	return &canonical, nil
}

func (s *Store) DeleteRuleOverride(_ context.Context, eventType eventrule.Type, scope eventrule.Scope) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for bindingID, persisted := range s.bindings {
		if !bindingRecordMatchesScope(persisted, eventType, scope) {
			continue
		}
		shared := false
		for otherID, other := range s.bindings {
			if otherID != bindingID && other.RuleID == persisted.RuleID {
				shared = true
				break
			}
		}
		delete(s.bindings, bindingID)
		if !shared {
			delete(s.rules, persisted.RuleID)
		}
		return nil
	}
	return nil
}
