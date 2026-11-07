// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Create ExpectedMachine table
		_, err := tx.NewCreateTable().Model((*model.ExpectedMachine)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_machine_site_id_idx")
		handleError(tx, err)

		// Add index for site_id
		_, err = tx.Exec("CREATE INDEX expected_machine_site_id_idx ON expected_machine(site_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_machine_bmc_mac_address_idx")
		handleError(tx, err)

		// Add index for bmc_mac_address (frequently queried)
		_, err = tx.Exec("CREATE INDEX expected_machine_bmc_mac_address_idx ON expected_machine(bmc_mac_address)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_machine_chassis_serial_number_idx")
		handleError(tx, err)

		// Add index for chassis_serial_number (frequently queried for hardware identification)
		_, err = tx.Exec("CREATE INDEX expected_machine_chassis_serial_number_idx ON expected_machine(chassis_serial_number)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_machine_created_idx")
		handleError(tx, err)

		// Add index for created timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_machine_created_idx ON expected_machine(created)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_machine_updated_idx")
		handleError(tx, err)

		// Add index for updated timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_machine_updated_idx ON expected_machine(updated)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Created 'expected_machine' table and created indices successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
