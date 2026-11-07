// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/uptrace/bun"
)

func init() {
	// migration to add a new column to the user table and set its value from ngc_data
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, nil)
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Add new column to user table
		_, err := tx.NewAddColumn().Model((*model.User)(nil)).IfNotExists().ColumnExpr("org_data JSONB NOT NULL DEFAULT ('{}')").Exec(ctx)
		handleError(tx, err)

		// Migrate ngc_data to org_data
		_, err = tx.NewUpdate().Model((*model.User)(nil)).
			Set("org_data = ngc_org_data").
			Where("ngc_org_data IS NOT NULL").
			Exec(ctx)
		handleError(tx, err)
		fmt.Println(" [up migration] Migrated ngc_org_data to org_data")

		/* Modify Starfleet ID column */
		// Drop unique constraint on starfleet_id
		_, err = tx.Exec("ALTER TABLE public.user DROP CONSTRAINT IF EXISTS user_starfleet_id_key")
		handleError(tx, err)

		// Make starfleet_id column nullable
		_, err = tx.Exec("ALTER TABLE public.user ALTER COLUMN starfleet_id DROP NOT NULL")
		handleError(tx, err)

		// Drop the incorrectly named starfleet_id index (typo fix)
		_, err = tx.Exec("DROP INDEX IF EXISTS user_startfleet_id_idx")
		handleError(tx, err)

		// Create partial unique index for starfleet_id when it's not null
		// This ensures uniqueness of non-null starfleet_id values
		_, err = tx.Exec(`ALTER TABLE public.user ADD CONSTRAINT user_starfleet_id_key UNIQUE (starfleet_id)`)
		handleError(tx, err)

		/* Modify Auxiliary ID column */
		// Drop the unique constraint on auxiliary_id
		_, err = tx.Exec("ALTER TABLE public.user DROP CONSTRAINT IF EXISTS user_auxiliary_id_key")
		handleError(tx, err)

		// Drop incorrectly named auxiliary_id index
		_, err = tx.Exec("DROP INDEX IF EXISTS user_auxiliary_id_indx")
		handleError(tx, err)

		// Switch empty string values to null
		_, err = tx.Exec("UPDATE public.user SET auxiliary_id = NULL WHERE auxiliary_id = ''")
		handleError(tx, err)

		// Create partial unique index for auxiliary_id when it's not null
		// This ensures uniqueness of non-null auxiliary_id values
		_, err = tx.Exec(`ALTER TABLE public.user ADD CONSTRAINT user_auxiliary_id_key UNIQUE (auxiliary_id)`)
		handleError(tx, err)

		err = tx.Commit()
		handleError(tx, err)

		fmt.Println(" [up migration] Completed user migration from ngc_org_data to org_data")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Println(" [down migration] No down migration implemented for user migration from ngc_org_data to org_data")
		return nil
	})
}
