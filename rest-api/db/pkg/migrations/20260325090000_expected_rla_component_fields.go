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

		// Add columns to expected_machine table
		_, err := tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS rack_id TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS name TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS manufacturer TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS model TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS description TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS firmware_version TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS slot_id INTEGER")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS tray_idx INTEGER")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS host_id INTEGER")
		handleError(tx, err)

		// Add columns to expected_power_shelf table
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS rack_id TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS name TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS manufacturer TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS model TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS description TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS firmware_version TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS slot_id INTEGER")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS tray_idx INTEGER")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_power_shelf ADD COLUMN IF NOT EXISTS host_id INTEGER")
		handleError(tx, err)

		// Add columns to expected_switch table
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS rack_id TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS name TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS manufacturer TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS model TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS description TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS firmware_version TEXT")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS slot_id INTEGER")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS tray_idx INTEGER")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_switch ADD COLUMN IF NOT EXISTS host_id INTEGER")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added RLA component fields to 'expected_machine', 'expected_power_shelf', and 'expected_switch' tables successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
