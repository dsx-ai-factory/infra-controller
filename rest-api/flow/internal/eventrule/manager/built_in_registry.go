// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
)

// builtInRegistry stores validated built-in rules. Its read methods accept a
// context to implement the shared store interfaces, although this in-memory
// implementation does not use it; persisted stores need it for their I/O.
type builtInRegistry struct {
	byID        map[uuid.UUID]eventrule.Rule
	byEventType map[eventrule.Type]uuid.UUID
}

func (r *builtInRegistry) addRule(rule *eventrule.Rule) error {
	if rule == nil {
		return fmt.Errorf("register built-in rule: event rule is nil")
	}

	if rule.Origin != eventrule.RuleOriginBuiltIn {
		return fmt.Errorf("register built-in rule %s: origin must be %q", rule.ID, eventrule.RuleOriginBuiltIn)
	}

	if err := rule.Validate(); err != nil {
		return fmt.Errorf("register built-in rule %s: %w", rule.ID, err)
	}

	if _, ok := r.byID[rule.ID]; ok {
		return fmt.Errorf("register built-in rule: duplicate id %s", rule.ID)
	}

	if _, ok := r.byEventType[rule.EventType]; ok {
		return fmt.Errorf("register built-in rule: duplicate event type %q", rule.EventType)
	}

	r.byID[rule.ID] = rule.Clone()
	r.byEventType[rule.EventType] = rule.ID

	return nil
}

// GetByID returns a built-in rule by stable UUID.
func (r *builtInRegistry) GetByID(_ context.Context, id uuid.UUID) (*eventrule.Rule, error) {
	rule, ok := r.byID[id]
	if !ok {
		return nil, fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
	}
	cloned := rule.Clone()
	return &cloned, nil
}

// GetByEventType returns the built-in fallback for an event type.
func (r *builtInRegistry) GetByEventType(_ context.Context, eventType eventrule.Type) (*eventrule.Rule, error) {
	id, ok := r.byEventType[eventType]
	if !ok {
		return nil, fmt.Errorf("%w: event type %q", eventrule.ErrRuleNotFound, eventType)
	}

	storedRule, ok := r.byID[id]
	if !ok {
		return nil, fmt.Errorf(
			"built-in event rule registry is inconsistent: event type %q references missing rule %s",
			eventType,
			id,
		)
	}

	rule := storedRule.Clone()
	return &rule, nil
}

// supportsEventType reports whether Flow registered a built-in fallback for
// the event type. Every supported event type must have one built-in rule.
func (r *builtInRegistry) supportsEventType(eventType eventrule.Type) bool {
	if r == nil {
		return false
	}

	_, ok := r.byEventType[eventType]
	return ok
}

// List returns one stable-ID-ordered page of matching built-in rules.
func (r *builtInRegistry) List(
	_ context.Context,
	request eventrule.RuleListRequest,
) (eventrule.RuleListPage, error) {
	if err := request.Validate(); err != nil {
		return eventrule.RuleListPage{}, err
	}

	rules := make([]*eventrule.Rule, 0, len(r.byID))
	for _, stored := range r.byID {
		if !request.Filter.Matches(&stored) {
			continue
		}
		rule := stored.Clone()
		rules = append(rules, &rule)
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

var _ eventrule.BuiltInRuleReader = (*builtInRegistry)(nil)
