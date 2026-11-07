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

		// Create ExpectedPowerShelf table
		_, err := tx.NewCreateTable().Model((*model.ExpectedPowerShelf)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_power_shelf_site_id_idx")
		handleError(tx, err)

		// Add index for site_id
		_, err = tx.Exec("CREATE INDEX expected_power_shelf_site_id_idx ON expected_power_shelf(site_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_power_shelf_bmc_mac_address_idx")
		handleError(tx, err)

		// Add index for bmc_mac_address (frequently queried)
		_, err = tx.Exec("CREATE INDEX expected_power_shelf_bmc_mac_address_idx ON expected_power_shelf(bmc_mac_address)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_power_shelf_shelf_serial_number_idx")
		handleError(tx, err)

		// Add index for shelf_serial_number (frequently queried for hardware identification)
		_, err = tx.Exec("CREATE INDEX expected_power_shelf_shelf_serial_number_idx ON expected_power_shelf(shelf_serial_number)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_power_shelf_created_idx")
		handleError(tx, err)

		// Add index for created timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_power_shelf_created_idx ON expected_power_shelf(created)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_power_shelf_updated_idx")
		handleError(tx, err)

		// Add index for updated timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_power_shelf_updated_idx ON expected_power_shelf(updated)")
		handleError(tx, err)

		// Create ExpectedSwitch table
		_, err = tx.NewCreateTable().Model((*model.ExpectedSwitch)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_switch_site_id_idx")
		handleError(tx, err)

		// Add index for site_id
		_, err = tx.Exec("CREATE INDEX expected_switch_site_id_idx ON expected_switch(site_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_switch_bmc_mac_address_idx")
		handleError(tx, err)

		// Add index for bmc_mac_address (frequently queried)
		_, err = tx.Exec("CREATE INDEX expected_switch_bmc_mac_address_idx ON expected_switch(bmc_mac_address)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_switch_switch_serial_number_idx")
		handleError(tx, err)

		// Add index for switch_serial_number (frequently queried for hardware identification)
		_, err = tx.Exec("CREATE INDEX expected_switch_switch_serial_number_idx ON expected_switch(switch_serial_number)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_switch_created_idx")
		handleError(tx, err)

		// Add index for created timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_switch_created_idx ON expected_switch(created)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_switch_updated_idx")
		handleError(tx, err)

		// Add index for updated timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_switch_updated_idx ON expected_switch(updated)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Created 'expected_power_shelf' and 'expected_switch' tables and created indices successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
