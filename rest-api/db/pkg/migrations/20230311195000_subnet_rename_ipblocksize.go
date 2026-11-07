// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func subnetIPBlockSizeRenameUpMigration(ctx context.Context, db *bun.DB) error {
	// Start transaction
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Rename ip_block_size to prefix_length if ip_block_size column exists
	res, err := tx.Exec("SELECT column_name FROM information_schema.columns WHERE table_name = 'subnet' AND column_name = 'ip_block_size'")
	handleError(tx, err)
	rowsAffected, err := res.RowsAffected()
	handleError(tx, err)
	if rowsAffected > 0 {
		_, err := tx.Exec("ALTER TABLE subnet RENAME COLUMN ip_block_size TO prefix_length")
		handleError(tx, err)
	}

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(subnetIPBlockSizeRenameUpMigration, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
