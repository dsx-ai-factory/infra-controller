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

		// Remove NOT NULL constraint from allocation_id in instance table
		// when the legacy column is still present.
		_, err := tx.Exec(`
			DO $$
			BEGIN
				ALTER TABLE instance ALTER COLUMN allocation_id DROP NOT NULL;
			EXCEPTION
				WHEN undefined_column THEN
					RAISE NOTICE 'Column allocation_id does not exist, skipping modification.';
			END $$;
		`)
		handleError(tx, err)

		// Remove NOT NULL constraint from allocation_constraint_id in instance table
		// when the legacy column is still present.
		_, err = tx.Exec(`
			DO $$
			BEGIN
				ALTER TABLE instance ALTER COLUMN allocation_constraint_id DROP NOT NULL;
			EXCEPTION
				WHEN undefined_column THEN
					RAISE NOTICE 'Column allocation_constraint_id does not exist, skipping modification.';
			END $$;
		`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
