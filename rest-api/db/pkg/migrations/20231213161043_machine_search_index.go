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

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err := tx.Exec("DROP INDEX IF EXISTS machine_gin_idx")
		handleError(tx, err)

		// Add GIN index for machine table
		_, err = tx.Exec("CREATE INDEX machine_gin_idx ON public.machine USING GIN (id gin_trgm_ops, vendor gin_trgm_ops, product_name gin_trgm_ops, hostname gin_trgm_ops, status gin_trgm_ops)")
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
