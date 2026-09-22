// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations_test

import (
	"context"
	"os"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/migrations"
	"github.com/stretchr/testify/require"
)

const eventRuleMigrationID = "20260827120000"

func TestEventRuleMigration(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping event-rule migration test: no DB environment specified")
	}

	ctx := context.Background()
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)

	session, err := utils.UnitTestDB(ctx, t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)

	assertEventRuleTables(t, ctx, session, true)

	future := time.Now().UTC().Add(time.Hour)
	_, err = session.DB.ExecContext(
		ctx,
		"UPDATE migrations SET applied_date = ? WHERE id = ?",
		future,
		eventRuleMigrationID,
	)
	require.NoError(t, err)

	require.NoError(t, migrations.RollbackWithDB(
		ctx,
		session.DB,
		future.Add(-time.Minute),
	))
	assertEventRuleTables(t, ctx, session, false)

	require.NoError(t, migrations.MigrateWithDB(
		ctx,
		session.DB,
		migrations.MigrateOptions{},
	))
	assertEventRuleTables(t, ctx, session, true)
}

func assertEventRuleTables(
	t *testing.T,
	ctx context.Context,
	session *cdb.Session,
	wantPresent bool,
) {
	t.Helper()

	for _, table := range []string{
		"event_rules",
		"event_rule_bindings",
		"events",
		"event_action_executions",
	} {
		var resolved *string
		err := session.DB.NewSelect().
			ColumnExpr("to_regclass(?)", "public."+table).
			Scan(ctx, &resolved)
		require.NoError(t, err)
		if wantPresent {
			require.NotNil(t, resolved, table)

			continue
		}

		require.Nil(t, resolved, table)
	}
}
