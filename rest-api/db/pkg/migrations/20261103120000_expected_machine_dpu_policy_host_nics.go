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
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Per-host DPU policy (REST dpuPolicy → Core dpu_mode).
		_, err := tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS dpu_policy TEXT")
		handleError(tx, err)

		// Declared host interfaces (REST hostNics → Core host_nics). Empty array
		// default preserves existing rows with no declared interfaces.
		_, err = tx.Exec(`ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS host_nics JSONB NOT NULL DEFAULT '[]'::jsonb`)
		handleError(tx, err)

		// Skip BMC password rotation when set (REST bmcRetainCredentials).
		_, err = tx.Exec("ALTER TABLE expected_machine ADD COLUMN IF NOT EXISTS bmc_retain_credentials BOOLEAN")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added dpu_policy, host_nics, and bmc_retain_credentials columns to 'expected_machine'. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
