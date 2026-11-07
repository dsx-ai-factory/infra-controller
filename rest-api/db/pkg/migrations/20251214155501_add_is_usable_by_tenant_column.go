// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Add new column is_usable_by_tenant
		// Keep the old column is_allocatable for backward compatibility with older versions of cloud-db
		// Machine inventory will populate the appropriate value (logic differs from is_allocatable)
		//
		// TODO: In a future migration (after all services migrate to IsUsableByTenant):
		//   1. Drop the deprecated is_allocatable column from the database
		//   2. Remove this TODO comment
		_, err := tx.ExecContext(ctx, "ALTER TABLE machine ADD COLUMN IF NOT EXISTS is_usable_by_tenant bool DEFAULT false")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration: added is_usable_by_tenant column] ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		// Rollback: drop the new column
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			return terr
		}

		_, err := tx.ExecContext(ctx, "ALTER TABLE machine DROP COLUMN IF EXISTS is_usable_by_tenant")
		if err != nil {
			tx.Rollback()
			return err
		}

		terr = tx.Commit()
		if terr != nil {
			return terr
		}

		fmt.Print(" [down migration] ")
		return nil
	})
}
