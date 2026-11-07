// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Add machine_id column to expected_machine table
		_, err := tx.NewAddColumn().Model((*model.ExpectedMachine)(nil)).IfNotExists().ColumnExpr("machine_id TEXT NULL").Exec(ctx)
		handleError(tx, err)

		// Add Machine foreign key for expected_machine
		// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("ALTER TABLE expected_machine DROP CONSTRAINT IF EXISTS expected_machine_machine_id_fkey")
		handleError(tx, err)

		_, err = tx.Exec("ALTER TABLE expected_machine ADD CONSTRAINT expected_machine_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS expected_machine_machine_id_idx")
		handleError(tx, err)

		// Add index for machine_id for improved query performance
		_, err = tx.Exec("CREATE INDEX expected_machine_machine_id_idx ON expected_machine(machine_id)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added machine_id column and foreign key to expected_machine table. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
