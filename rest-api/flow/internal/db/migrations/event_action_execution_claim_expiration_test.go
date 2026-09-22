// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations_test

import (
	"context"
	_ "embed"
	"fmt"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

//go:embed 20260829000000_add_event_action_execution_claim_expiration.up.sql
var addEventActionExecutionClaimExpirationUp string

//go:embed 20260829000000_add_event_action_execution_claim_expiration.down.sql
var addEventActionExecutionClaimExpirationDown string

func TestEventActionExecutionClaimExpirationMigration(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)

	_, err := session.DB.ExecContext(ctx, addEventActionExecutionClaimExpirationDown)
	require.NoError(t, err)

	eventID := uuid.New()
	now := time.Date(2026, time.August, 29, 12, 0, 0, 0, time.UTC)
	_, err = session.DB.ExecContext(
		ctx,
		`INSERT INTO events (
			id,
			source_name,
			source_key,
			event_type,
			resource_id,
			resource_kind,
			applied_rule_id,
			effective_policy,
			summary,
			observations,
			created_at,
			last_observed_at
		) VALUES (?, 'test', ?, 'test.event', ?, 'rack', ?, '{}', 'test event', 1, ?, ?)`,
		eventID,
		uuid.NewString(),
		uuid.New(),
		uuid.New(),
		now,
		now,
	)
	require.NoError(t, err)

	legacyExecutionID := uuid.New()
	_, err = session.DB.ExecContext(
		ctx,
		`INSERT INTO event_action_executions (
			id,
			event_id,
			action_name,
			action_type,
			plan,
			status,
			attempts,
			claim_token,
			claim_owner,
			created_at,
			updated_at
		) VALUES (?, ?, 'legacy', 'noop', '{}', 'running', 1, ?, 'worker', ?, ?)`,
		legacyExecutionID,
		eventID,
		uuid.New(),
		now,
		now,
	)
	require.NoError(t, err)

	_, err = session.DB.ExecContext(ctx, addEventActionExecutionClaimExpirationUp)
	require.NoError(t, err)

	var validated bool
	err = session.DB.NewSelect().
		TableExpr("pg_constraint").
		Column("convalidated").
		Where("conname = ?", "event_action_executions_claim_expiration_check").
		Where("conrelid = ?::regclass", "event_action_executions").
		Scan(ctx, &validated)
	require.NoError(t, err)
	require.False(t, validated)

	tests := []struct {
		name           string
		status         string
		attempts       int
		claimToken     any
		claimOwner     any
		claimExpiresAt any
		wantValid      bool
	}{
		{
			name:           "running with future expiration",
			status:         "running",
			attempts:       1,
			claimToken:     uuid.New(),
			claimOwner:     "worker",
			claimExpiresAt: now.Add(time.Minute),
			wantValid:      true,
		},
		{
			name:       "running without expiration",
			status:     "running",
			attempts:   1,
			claimToken: uuid.New(),
			claimOwner: "worker",
		},
		{
			name:           "running with expiration equal to update",
			status:         "running",
			attempts:       1,
			claimToken:     uuid.New(),
			claimOwner:     "worker",
			claimExpiresAt: now,
		},
		{
			name:           "pending with expiration",
			status:         "pending",
			claimExpiresAt: now.Add(time.Minute),
		},
		{
			name:      "pending without expiration",
			status:    "pending",
			wantValid: true,
		},
	}

	for i, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := session.DB.ExecContext(
				ctx,
				`INSERT INTO event_action_executions (
					id,
					event_id,
					action_name,
					action_type,
					plan,
					status,
					attempts,
					claim_token,
					claim_owner,
					claim_expires_at,
					created_at,
					updated_at
				) VALUES (?, ?, ?, 'noop', '{}', ?, ?, ?, ?, ?, ?, ?)`,
				uuid.New(),
				eventID,
				fmt.Sprintf("case_%d", i),
				test.status,
				test.attempts,
				test.claimToken,
				test.claimOwner,
				test.claimExpiresAt,
				now,
				now,
			)
			if test.wantValid {
				require.NoError(t, err)
				return
			}

			require.ErrorContains(
				t,
				err,
				"event_action_executions_claim_expiration_check",
			)
		})
	}

	_, err = session.DB.ExecContext(
		ctx,
		"DELETE FROM event_action_executions WHERE id = ?",
		legacyExecutionID,
	)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(
		ctx,
		`ALTER TABLE event_action_executions
			VALIDATE CONSTRAINT event_action_executions_claim_expiration_check`,
	)
	require.NoError(t, err)
}
