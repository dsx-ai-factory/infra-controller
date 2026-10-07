// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

// EventActionExecution is the persistence model for one immutable event action
// plan and its mutable dispatch state.
type EventActionExecution struct {
	bun.BaseModel `bun:"table:event_action_executions,alias:eae"`

	ID             uuid.UUID       `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	EventID        uuid.UUID       `bun:"event_id,type:uuid,notnull"`
	ActionName     string          `bun:"action_name,notnull"`
	ActionType     string          `bun:"action_type,notnull"`
	Plan           json.RawMessage `bun:"plan,type:jsonb,notnull"`
	Status         string          `bun:"status,notnull"`
	Reason         *string         `bun:"reason"`
	Attempts       int             `bun:"attempts,notnull"`
	ClaimToken     *uuid.UUID      `bun:"claim_token,type:uuid"`
	ClaimOwner     *string         `bun:"claim_owner"`
	ClaimExpiresAt *time.Time      `bun:"claim_expires_at"`
	StatusMessage  *string         `bun:"status_message"`
	CreatedAt      time.Time       `bun:"created_at,notnull"`
	UpdatedAt      time.Time       `bun:"updated_at,notnull"`
	NextAttemptAt  *time.Time      `bun:"next_attempt_at"`
}
