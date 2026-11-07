// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"os"
	"testing"
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db/postgres"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db/testutil"
)

// skipIfNoDatabase skips the test if database environment variables are not set.
func skipIfNoDatabase(t *testing.T) {
	t.Helper()
	if os.Getenv("DB_PORT") == "" {
		log.Warn("Skipping integration test: DB_PORT not set")
		t.Skip("DB_PORT environment variable not set - skipping integration test")
	}
}

// setupTestDB creates a fresh test database for the migration tests.
func setupTestDB(t *testing.T) (*postgres.Postgres, func()) {
	t.Helper()
	ctx := context.Background()

	dbConf, err := db.BuildDBConfigFromEnv()
	require.NoError(t, err, "Failed to build DB config from env")

	pg, err := testutil.CreateTestDB(ctx, t, dbConf)
	require.NoError(t, err, "Failed to create test database")

	cleanup := func() {
		pg.Close(ctx)
	}

	return pg, cleanup
}

// TestIntegration_Migrate_CreatesTablesAndRecordsMigration tests the basic migration flow.
func TestIntegration_Migrate_CreatesTablesAndRecordsMigration(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations
	err := Migrate(ctx, pg)
	require.NoError(t, err, "Migrate should succeed")

	// Verify migrations table was created and has entries
	var count int
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		ColumnExpr("COUNT(*)").
		Scan(ctx, &count)
	require.NoError(t, err, "Should be able to query migrations table")
	assert.Greater(t, count, 0, "At least one migration should be recorded")

	// Verify the nvswitch table was created (from initial migration)
	var tableExists bool
	err = pg.DB().NewSelect().
		TableExpr("information_schema.tables").
		ColumnExpr("TRUE").
		Where("table_name = ?", "nvswitch").
		Where("table_schema = ?", "public").
		Scan(ctx, &tableExists)
	require.NoError(t, err, "Should be able to check for nvswitch table")
	assert.True(t, tableExists, "nvswitch table should exist after migration")

	// Verify the firmware_update table was created
	err = pg.DB().NewSelect().
		TableExpr("information_schema.tables").
		ColumnExpr("TRUE").
		Where("table_name = ?", "firmware_update").
		Where("table_schema = ?", "public").
		Scan(ctx, &tableExists)
	require.NoError(t, err, "Should be able to check for firmware_update table")
	assert.True(t, tableExists, "firmware_update table should exist after migration")
}

// TestIntegration_Migrate_IsIdempotent tests that running migrations multiple times is safe.
func TestIntegration_Migrate_IsIdempotent(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations first time
	err := Migrate(ctx, pg)
	require.NoError(t, err, "First Migrate should succeed")

	// Get count of applied migrations
	var countBefore int
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		ColumnExpr("COUNT(*)").
		Scan(ctx, &countBefore)
	require.NoError(t, err)

	// Run migrations second time - should be idempotent
	err = Migrate(ctx, pg)
	require.NoError(t, err, "Second Migrate should succeed (idempotent)")

	// Verify count is the same
	var countAfter int
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		ColumnExpr("COUNT(*)").
		Scan(ctx, &countAfter)
	require.NoError(t, err)

	assert.Equal(t, countBefore, countAfter, "Migration count should not change on re-run")
}

// TestIntegration_Migrate_RecordsHash tests that migration hashes are correctly stored.
func TestIntegration_Migrate_RecordsHash(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Verify hash is stored for the initial nvswitch migration
	var hash string
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		Column("hash").
		Where("id = ?", "202501290000").
		Scan(ctx, &hash)
	require.NoError(t, err, "Should be able to query migration hash")
	assert.NotEmpty(t, hash, "Migration hash should be recorded")
	assert.Len(t, hash, 32, "Hash should be MD5 hex (32 chars)")
}

// TestIntegration_Rollback_RemovesMigration tests the rollback functionality.
func TestIntegration_Rollback_RemovesMigration(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations first
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Get the applied date of the initial nvswitch migration
	var appliedDate time.Time
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		Column("applied_date").
		Where("id = ?", "202501290000").
		Scan(ctx, &appliedDate)
	require.NoError(t, err)

	// Rollback to before the migration was applied
	rollbackTime := appliedDate.Add(-1 * time.Second)
	err = Rollback(ctx, pg, rollbackTime)
	require.NoError(t, err, "Rollback should succeed")

	// Verify migration record was removed
	var count int
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		ColumnExpr("COUNT(*)").
		Where("id = ?", "202501290000").
		Scan(ctx, &count)
	require.NoError(t, err)
	assert.Equal(t, 0, count, "Migration should be removed after rollback")

	// Verify nvswitch table was dropped
	var tableExists bool
	err = pg.DB().NewSelect().
		TableExpr("information_schema.tables").
		ColumnExpr("TRUE").
		Where("table_name = ?", "nvswitch").
		Where("table_schema = ?", "public").
		Scan(ctx, &tableExists)
	// If no rows, tableExists remains false
	if err != nil {
		tableExists = false
	}
	assert.False(t, tableExists, "nvswitch table should be dropped after rollback")
}

// TestIntegration_Rollback_NoOpWhenNothingToRollback tests rollback with future time.
func TestIntegration_Rollback_NoOpWhenNothingToRollback(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations first
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Get count before rollback
	var countBefore int
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		ColumnExpr("COUNT(*)").
		Scan(ctx, &countBefore)
	require.NoError(t, err)

	// Rollback to a time far in the past (before any migrations)
	// Actually, let's use current time which should be after all migrations
	// This means nothing should be rolled back
	err = Rollback(ctx, pg, time.Now().Add(1*time.Hour))
	require.NoError(t, err, "Rollback should succeed even with nothing to rollback")

	// Count should be the same
	var countAfter int
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		ColumnExpr("COUNT(*)").
		Scan(ctx, &countAfter)
	require.NoError(t, err)

	assert.Equal(t, countBefore, countAfter, "No migrations should be rolled back")
}

// TestIntegration_Migrate_AfterRollback tests re-applying migrations after rollback.
func TestIntegration_Migrate_AfterRollback(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Initial migration
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Get applied date of initial nvswitch migration
	var appliedDate time.Time
	err = pg.DB().NewSelect().
		TableExpr("migrations").
		Column("applied_date").
		Where("id = ?", "202501290000").
		Scan(ctx, &appliedDate)
	require.NoError(t, err)

	// Rollback
	rollbackTime := appliedDate.Add(-1 * time.Second)
	err = Rollback(ctx, pg, rollbackTime)
	require.NoError(t, err)

	// Re-apply migrations
	err = Migrate(ctx, pg)
	require.NoError(t, err, "Re-applying migrations after rollback should succeed")

	// Verify tables exist again
	var tableExists bool
	err = pg.DB().NewSelect().
		TableExpr("information_schema.tables").
		ColumnExpr("TRUE").
		Where("table_name = ?", "nvswitch").
		Where("table_schema = ?", "public").
		Scan(ctx, &tableExists)
	require.NoError(t, err)
	assert.True(t, tableExists, "nvswitch table should exist after re-migration")
}

// TestIntegration_LockOrCreateMigrationTable tests the locking mechanism.
func TestIntegration_LockOrCreateMigrationTable(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// The real test is that Migrate succeeds (which uses locking internally)
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Verify migrations table has correct schema
	var columns []string
	rows, err := pg.DB().QueryContext(ctx, `
		SELECT column_name 
		FROM information_schema.columns 
		WHERE table_name = 'migrations' 
		ORDER BY ordinal_position
	`)
	require.NoError(t, err)
	defer rows.Close()

	for rows.Next() {
		var col string
		require.NoError(t, rows.Scan(&col))
		columns = append(columns, col)
	}

	assert.Contains(t, columns, "id", "migrations table should have id column")
	assert.Contains(t, columns, "name", "migrations table should have name column")
	assert.Contains(t, columns, "hash", "migrations table should have hash column")
	assert.Contains(t, columns, "applied_date", "migrations table should have applied_date column")
}

// TestIntegration_ApplyMigration_SQLSectionSplit tests that SQL with SECTION markers works.
func TestIntegration_ApplyMigration_SQLSectionSplit(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations - the initial migration has SECTION markers
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Verify both tables were created (they're in different SECTIONs)
	var nvswitchExists, fwExists bool

	err = pg.DB().NewSelect().
		TableExpr("information_schema.tables").
		ColumnExpr("TRUE").
		Where("table_name = ?", "nvswitch").
		Scan(ctx, &nvswitchExists)
	require.NoError(t, err)

	err = pg.DB().NewSelect().
		TableExpr("information_schema.tables").
		ColumnExpr("TRUE").
		Where("table_name = ?", "firmware_update").
		Scan(ctx, &fwExists)
	require.NoError(t, err)

	assert.True(t, nvswitchExists, "nvswitch table (first SECTION) should exist")
	assert.True(t, fwExists, "firmware_update table (second SECTION) should exist")
}

// TestIntegration_Indexes_Created tests that indexes are created by migrations.
func TestIntegration_Indexes_Created(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Check for expected indexes
	expectedIndexes := []string{
		"nvswitch_vendor_idx",
		"firmware_update_state_idx",
		"firmware_update_created_at_idx",
		"firmware_update_state_created_idx",
	}

	for _, indexName := range expectedIndexes {
		var exists bool
		err := pg.DB().NewSelect().
			TableExpr("pg_indexes").
			ColumnExpr("TRUE").
			Where("indexname = ?", indexName).
			Scan(ctx, &exists)

		if err != nil {
			exists = false
		}
		assert.True(t, exists, "Index %s should exist", indexName)
	}
}

// TestIntegration_MacaddrAndInetTypes tests that native PostgreSQL types are used.
func TestIntegration_MacaddrAndInetTypes(t *testing.T) {
	skipIfNoDatabase(t)
	ctx := context.Background()
	pg, cleanup := setupTestDB(t)
	defer cleanup()

	// Run migrations
	err := Migrate(ctx, pg)
	require.NoError(t, err)

	// Check column types for nvswitch table
	type columnInfo struct {
		ColumnName string
		DataType   string
	}

	var columns []columnInfo
	rows, err := pg.DB().QueryContext(ctx, `
		SELECT column_name, data_type 
		FROM information_schema.columns 
		WHERE table_name = 'nvswitch' AND table_schema = 'public'
	`)
	require.NoError(t, err)
	defer rows.Close()

	for rows.Next() {
		var col columnInfo
		require.NoError(t, rows.Scan(&col.ColumnName, &col.DataType))
		columns = append(columns, col)
	}

	// Find bmc_mac_address and bmc_ip_address columns
	var macType, ipType string
	for _, col := range columns {
		if col.ColumnName == "bmc_mac_address" {
			macType = col.DataType
		}
		if col.ColumnName == "bmc_ip_address" {
			ipType = col.DataType
		}
	}

	assert.Equal(t, "macaddr", macType, "bmc_mac_address should use macaddr type")
	assert.Equal(t, "inet", ipType, "bmc_ip_address should use inet type")
}
