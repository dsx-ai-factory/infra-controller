// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func tenantConfigUpMigration(ctx context.Context, db *bun.DB) error {
	// Start transactions
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Remove NOT NULL constraint from instance_type_id in instance table
	_, err := tx.Exec("ALTER TABLE instance ALTER COLUMN instance_type_id DROP NOT NULL;")
	handleError(tx, err)

	// Ensure existing column will get an empty JSON as default value
	_, err = tx.Exec("ALTER TABLE tenant ALTER COLUMN config SET DEFAULT '{}'::jsonb")
	handleError(tx, err)

	// Set the config column in tenant table to {}::jsonb
	_, err = tx.Exec("UPDATE tenant SET config='{}'::jsonb WHERE config IS NULL")
	handleError(tx, err)

	// Set the config column in tenant table to not null
	_, err = tx.Exec("ALTER TABLE tenant ALTER COLUMN config SET NOT NULL")
	handleError(tx, err)

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(tenantConfigUpMigration, func(_ context.Context, _ *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
