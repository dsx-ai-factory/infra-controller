// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

// SpecType identifies the scheduling mechanism for a task schedule.
type SpecType string

const (
	SpecTypeInterval SpecType = "interval"
	SpecTypeCron     SpecType = "cron"
	SpecTypeOneTime  SpecType = "one-time"
)

// OverlapPolicy controls what happens when a schedule fires but the previous
// execution is still active.
type OverlapPolicy string

const (
	OverlapPolicySkip  OverlapPolicy = "skip"
	OverlapPolicyQueue OverlapPolicy = "queue"
)

// TaskSchedule is the bun model for the task_schedule table.
type TaskSchedule struct {
	bun.BaseModel `bun:"table:task_schedule,alias:ts"`

	ID                uuid.UUID       `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Name              string          `bun:"name,notnull"`
	SpecType          SpecType        `bun:"spec_type,type:varchar(16),notnull"`
	Spec              string          `bun:"spec,notnull"`
	Timezone          string          `bun:"timezone,notnull"`
	OperationTemplate json.RawMessage `bun:"operation_template,type:jsonb,notnull"`
	OverlapPolicy     OverlapPolicy   `bun:"overlap_policy,type:varchar(16),notnull"`
	Enabled           bool            `bun:"enabled,notnull"`
	NextRunAt         *time.Time      `bun:"next_run_at"`
	LastRunAt         *time.Time      `bun:"last_run_at"`
	CreatedAt         time.Time       `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt         time.Time       `bun:"updated_at,notnull,default:current_timestamp"`
}
