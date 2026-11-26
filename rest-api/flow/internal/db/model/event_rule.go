// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

// EventRule is the bun model for the event_rules table.
type EventRule struct {
	bun.BaseModel `bun:"table:event_rules,alias:er"`

	ID          uuid.UUID       `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Name        string          `bun:"name,notnull"`
	Description string          `bun:"description,notnull"`
	Enabled     bool            `bun:"enabled,notnull"`
	EventType   string          `bun:"event_type,notnull"`
	Policy      json.RawMessage `bun:"policy,type:jsonb,notnull"`
	CreatedAt   time.Time       `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt   time.Time       `bun:"updated_at,notnull,default:current_timestamp"`
}

// EventRuleBinding is the bun model for the event_rule_bindings table.
type EventRuleBinding struct {
	bun.BaseModel `bun:"table:event_rule_bindings,alias:erb"`

	ID        uuid.UUID  `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	RuleID    uuid.UUID  `bun:"rule_id,type:uuid,notnull"`
	EventType string     `bun:"event_type,notnull"`
	ScopeType string     `bun:"scope_type,notnull"`
	ScopeID   *uuid.UUID `bun:"scope_id,type:uuid"`
	CreatedAt time.Time  `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt time.Time  `bun:"updated_at,notnull,default:current_timestamp"`
}

// DeleteEventRuleBindingsForRack removes assignments while preserving shared policies.
// Callers must hold the rack row's mutation lock in the same transaction.
func DeleteEventRuleBindingsForRack(ctx context.Context, idb bun.IDB, rackID uuid.UUID) error {
	_, err := idb.NewDelete().Model((*EventRuleBinding)(nil)).
		Where("scope_type = ?", "rack").
		Where("scope_id = ?", rackID).
		Exec(ctx)
	if err != nil {
		return fmt.Errorf("delete event rule bindings for rack %s: %w", rackID, err)
	}
	return nil
}
