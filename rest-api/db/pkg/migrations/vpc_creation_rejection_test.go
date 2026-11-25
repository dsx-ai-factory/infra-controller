// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/migrate"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

// TestVpcCreationRejectionMigration proves a rejected rollback preserves both
// cancellation decisions and the migration record used by the CLI.
func TestVpcCreationRejectionMigration(t *testing.T) {
	ctx := context.Background()
	session := util.GetTestDBSession(t, true)
	defer session.Close()

	// Only the VPC identity is needed before this nullable column is added.
	_, err := session.DB.ExecContext(ctx, "CREATE TABLE vpc (id UUID PRIMARY KEY)")
	require.NoError(t, err)
	id := uuid.New()
	_, err = session.DB.ExecContext(ctx, "INSERT INTO vpc (id) VALUES (?)", id)
	require.NoError(t, err)
	target := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20261112213316" {
			target.Add(migration)
		}
	}
	require.Len(t, target.Sorted(), 1)
	// Match the CLI: failed down migrations must remain recorded as applied.
	migrator := migrate.NewMigrator(session.DB, target, migrate.WithMarkAppliedOnSuccess(true))
	require.NoError(t, migrator.Init(ctx))
	_, err = migrator.Migrate(ctx)
	require.NoError(t, err)
	// Adding cancellation tracking must not reject VPCs that already exist.
	var unrejected bool
	err = session.DB.QueryRowContext(ctx, "SELECT creation_rejected_at IS NULL FROM vpc WHERE id = ?", id).Scan(&unrejected)
	require.NoError(t, err)
	require.True(t, unrejected)

	var before time.Time
	err = session.DB.QueryRowContext(ctx, "UPDATE vpc SET creation_rejected_at = CURRENT_TIMESTAMP WHERE id = ? RETURNING creation_rejected_at", id).Scan(&before)
	require.NoError(t, err)

	_, err = migrator.Rollback(ctx)
	require.ErrorContains(t, err, "VPC creation rejection decisions cannot be rolled back")
	var after time.Time
	err = session.DB.QueryRowContext(ctx, "SELECT creation_rejected_at FROM vpc WHERE id = ?", id).Scan(&after)
	require.NoError(t, err)
	require.Equal(t, before, after)
	status, err := migrator.MigrationsWithStatus(ctx)
	require.NoError(t, err)
	require.Len(t, status, 1)
	require.True(t, status[0].IsApplied())
}
