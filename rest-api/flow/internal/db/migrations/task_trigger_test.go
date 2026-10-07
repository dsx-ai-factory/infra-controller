// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations_test

import (
	"context"
	"os"
	"testing"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestTaskTriggerMigration(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping task-trigger migration test: no DB environment specified")
	}

	ctx := context.Background()
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)

	session, err := utils.UnitTestDB(ctx, t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)

	var validated bool
	err = session.DB.NewSelect().
		TableExpr("pg_constraint").
		Column("convalidated").
		Where("conname = ?", "task_trigger_complete").
		Where("conrelid = ?::regclass", "task").
		Scan(ctx, &validated)
	require.NoError(t, err)
	require.False(t, validated)

	tests := []struct {
		name        string
		triggerType any
		triggerID   any
		wantValid   bool
	}{
		{name: "no trigger", wantValid: true},
		{name: "API", triggerType: "api", wantValid: true},
		{
			name:        "event-rule execution",
			triggerType: "event_rule_execution",
			triggerID:   uuid.New(),
			wantValid:   true,
		},
		{name: "ID without type", triggerID: uuid.New()},
		{name: "API with ID", triggerType: "api", triggerID: uuid.New()},
		{name: "event-rule execution without ID", triggerType: "event_rule_execution"},
		{
			name:        "event-rule execution with zero ID",
			triggerType: "event_rule_execution",
			triggerID:   uuid.Nil,
		},
		{name: "unknown type", triggerType: "unknown"},
		{name: "unknown type with ID", triggerType: "unknown", triggerID: uuid.New()},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := session.DB.ExecContext(
				ctx,
				`INSERT INTO task (
					id,
					type,
					executor_type,
					rack_id,
					execution_id,
					status,
					trigger_type,
					trigger_id
				) VALUES (?, 'test', 'test', ?, ?, 'pending', ?, ?)`,
				uuid.New(),
				uuid.New(),
				uuid.NewString(),
				test.triggerType,
				test.triggerID,
			)
			if test.wantValid {
				require.NoError(t, err)
				return
			}

			require.ErrorContains(t, err, "task_trigger_complete")
		})
	}
}
