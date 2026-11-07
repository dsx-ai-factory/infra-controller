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
			// machine
			{index: "machine_updated_idx", table: "public.machine", column: "updated"},
			{index: "machine_deleted_idx", table: "public.machine", column: "deleted"},
			{index: "machine_site_id_idx", table: "public.machine", column: "site_id"},
			{index: "machine_instance_type_id_idx", table: "public.machine", column: "instance_type_id"},
			// machine capability
			{index: "machine_capability_type_idx", table: "public.machine_capability", column: "type"},
			{index: "machine_capability_name_idx", table: "public.machine_capability", column: "name"},
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
