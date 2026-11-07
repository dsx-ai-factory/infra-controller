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
		// Start transaction
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Add bmc_ip_address column to expected_machine table
		_, err := tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS bmc_ip_address TEXT")
		handleError(tx, err)

		// Add bmc_ip_address column to expected_switch table
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS bmc_ip_address TEXT")
		handleError(tx, err)

		// Rename ip_address to bmc_ip_address on expected_power_shelf table.
		// Guarded so it's idempotent + scoped to current_schema():
		//   - rename only when ip_address still exists, AND
		//   - bmc_ip_address does NOT yet exist (so a partial re-run is a no-op).
		_, err = tx.Exec(`
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'expected_power_shelf'
          AND column_name = 'ip_address'
    ) AND NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'expected_power_shelf'
          AND column_name = 'bmc_ip_address'
    ) THEN
        ALTER TABLE expected_power_shelf RENAME COLUMN ip_address TO bmc_ip_address;
    END IF;
END $$;
`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added bmc_ip_address column to 'expected_machine' and 'expected_switch'; renamed 'ip_address' to 'bmc_ip_address' on 'expected_power_shelf'. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
