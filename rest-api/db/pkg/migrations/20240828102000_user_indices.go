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
		indexes := []struct {
			index  string
			table  string
			column string
		}{
			{index: "user_email_idx", table: "public.user", column: "email"},
			{index: "user_first_name_idx", table: "public.user", column: "first_name"},
			{index: "user_last_name_idx", table: "public.user", column: "last_name"},
			{index: "user_created_idx", table: "public.user", column: "created"},
			{index: "user_updated_idx", table: "public.user", column: "updated"},
		}

		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		for _, idx := range indexes {
			// drop index (won't occur/harmless in dev/stage/prod but helps with test)
			_, err := tx.Exec(fmt.Sprintf("DROP INDEX IF EXISTS %s", idx.index))
			handleError(tx, err)

			// add index
			_, err = tx.Exec(fmt.Sprintf("CREATE INDEX %s ON %s(%s)", idx.index, idx.table, idx.column))
			handleError(tx, err)
		}

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
