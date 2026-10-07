// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

// Event is the bun model for the durable events table.
type Event struct {
	bun.BaseModel `bun:"table:events,alias:e"`

	ID              uuid.UUID       `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	SourceName      string          `bun:"source_name,notnull"`
	SourceKey       string          `bun:"source_key,notnull"`
	EventType       string          `bun:"event_type,notnull"`
	ResourceID      uuid.UUID       `bun:"resource_id,type:uuid,notnull"`
	ResourceKind    string          `bun:"resource_kind,notnull"`
	AppliedRuleID   uuid.UUID       `bun:"applied_rule_id,type:uuid,notnull"`
	EffectivePolicy json.RawMessage `bun:"effective_policy,type:jsonb,notnull"`
	Summary         string          `bun:"summary,notnull"`
	Observations    int             `bun:"observations,notnull"`
	CreatedAt       time.Time       `bun:"created_at,notnull"`
	LastObservedAt  time.Time       `bun:"last_observed_at,notnull"`
}
