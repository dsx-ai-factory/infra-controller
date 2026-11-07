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
		// Start transaction
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Create ExpectedRack table
		_, err := tx.NewCreateTable().Model((*model.ExpectedRack)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop unique constraint if it exists (for idempotency)
		_, err = tx.Exec("ALTER TABLE expected_rack DROP CONSTRAINT IF EXISTS expected_rack_rack_id_site_id_key")
		handleError(tx, err)

		// Add unique constraint on (rack_id, site_id) so the same rack_id may be
		// used in different sites but is unique within a site
		_, err = tx.Exec("ALTER TABLE expected_rack ADD CONSTRAINT expected_rack_rack_id_site_id_key UNIQUE (rack_id, site_id) DEFERRABLE INITIALLY DEFERRED")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_rack_site_id_idx")
		handleError(tx, err)

		// Add index for site_id (frequently queried for site-scoped lookups)
		_, err = tx.Exec("CREATE INDEX expected_rack_site_id_idx ON expected_rack(site_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_rack_rack_id_idx")
		handleError(tx, err)

		// Add index for rack_id (frequently queried for joins with expected_machine/switch/power_shelf)
		_, err = tx.Exec("CREATE INDEX expected_rack_rack_id_idx ON expected_rack(rack_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_rack_rack_profile_id_idx")
		handleError(tx, err)

		// Add index for rack_profile_id (frequently queried)
		_, err = tx.Exec("CREATE INDEX expected_rack_rack_profile_id_idx ON expected_rack(rack_profile_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_rack_name_idx")
		handleError(tx, err)

		// Add index for name (frequently queried for human-readable lookups)
		_, err = tx.Exec("CREATE INDEX expected_rack_name_idx ON expected_rack(name)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_rack_created_idx")
		handleError(tx, err)

		// Add index for created timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_rack_created_idx ON expected_rack(created)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_rack_updated_idx")
		handleError(tx, err)

		// Add index for updated timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX expected_rack_updated_idx ON expected_rack(updated)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Created 'expected_rack' table and created indices successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
