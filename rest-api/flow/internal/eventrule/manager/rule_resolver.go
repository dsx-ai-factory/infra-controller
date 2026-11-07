// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
)

// ruleResolver resolves the internal rack, site, and built-in precedence used
// while processing events.
type ruleResolver struct {
	builtIns *builtInRegistry
	store    eventrule.Store
}

// GetEffective returns the effective processing rule or (nil, nil) when no
// configured rule applies.
func (r *ruleResolver) GetEffective(
	ctx context.Context,
	eventType eventrule.Type,
	rackID uuid.UUID,
) (*eventrule.Rule, error) {
	// Prefer an enabled persisted rule bound to the event's rack.
	if rackID != uuid.Nil {
		scope := eventrule.Scope{
			Type: eventrule.ScopeTypeRack,
			ID:   rackID,
		}

		rule, err := r.getForScope(ctx, eventType, scope)
		if err != nil {
			return nil, err
		}

		if rule != nil {
			return rule, nil
		}
	}

	// Fall back to an enabled persisted rule bound to the site.
	scope := eventrule.Scope{
		Type: eventrule.ScopeTypeSite,
		ID:   uuid.Nil,
	}

	rule, err := r.getForScope(ctx, eventType, scope)
	if err != nil {
		return nil, err
	}

	if rule != nil {
		return rule, nil
	}

	// Use the immutable built-in when no persisted scope supplies a rule.
	rule, err = r.builtIns.GetByEventType(ctx, eventType)
	if errors.Is(err, eventrule.ErrRuleNotFound) {
		return nil, nil
	}

	return rule, err
}

func (r *ruleResolver) getForScope(
	ctx context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) (*eventrule.Rule, error) {
	binding, err := r.store.GetForScope(ctx, eventType, scope)
	if err != nil || binding == nil {
		return nil, err
	}

	rule, err := r.store.GetByID(ctx, binding.RuleID)
	if err != nil {
		return nil, err
	}

	if !rule.Enabled {
		return nil, nil
	}

	return rule, nil
}
