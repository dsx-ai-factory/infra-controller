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

		// Drop unique constraint if it exists (for idempotency)
		_, err := tx.Exec("ALTER TABLE expected_machine DROP CONSTRAINT IF EXISTS expected_machine_bmc_mac_address_site_id_key")
		handleError(tx, err)

		// Add deferrable unique constraint for bmc_mac_address and site_id combination
		// This ensures that within a site, each BMC MAC address is unique
		// DEFERRABLE INITIALLY DEFERRED allows constraint checks to be deferred until transaction commit,
		// enabling batch operations like MAC address swaps without intermediate violations
		_, err = tx.Exec("ALTER TABLE expected_machine ADD CONSTRAINT expected_machine_bmc_mac_address_site_id_key UNIQUE (bmc_mac_address, site_id) DEFERRABLE INITIALLY DEFERRED")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added deferrable unique constraint on (bmc_mac_address, site_id) for expected_machine table. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
