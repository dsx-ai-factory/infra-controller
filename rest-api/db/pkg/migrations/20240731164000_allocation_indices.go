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
			// allocation
			{index: "allocation_name_idx", table: "public.allocation", column: "name"},
			{index: "allocation_deleted_idx", table: "public.allocation", column: "deleted"},
			// allocation_constraint
			{index: "allocation_constraint_resource_type_idx", table: "public.allocation_constraint", column: "resource_type"},
			{index: "allocation_constraint_constraint_type_idx", table: "public.allocation_constraint", column: "constraint_type"},
			{index: "allocation_constraint_constraint_value_idx", table: "public.allocation_constraint", column: "constraint_value"},
			{index: "allocation_constraint_deleted_idx", table: "public.allocation_constraint", column: "deleted"},
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
