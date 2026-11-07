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

		// Add machine_id foreign key constraint
		// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err := tx.Exec("ALTER TABLE machine_capability DROP CONSTRAINT IF EXISTS machine_capability_machine_id_fkey")
		handleError(tx, err)

		_, err = tx.Exec("ALTER TABLE machine_capability ADD CONSTRAINT machine_capability_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
		handleError(tx, err)

		// Add instance_type_id foreign key constraint
		// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("ALTER TABLE machine_capability DROP CONSTRAINT IF EXISTS machine_capability_instance_type_id_fkey")
		handleError(tx, err)

		_, err = tx.Exec("ALTER TABLE machine_capability ADD CONSTRAINT machine_capability_instance_type_id_fkey FOREIGN KEY (instance_type_id) REFERENCES public.instance_type(id)")
		handleError(tx, err)

		// Add name column to MachineCapability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("name TEXT NOT NULL DEFAULT ''").Exec(ctx)
		handleError(tx, err)

		// Add frequency column to MachineCapability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("frequency TEXT").Exec(ctx)
		handleError(tx, err)

		// Add capacity column to MachineCapability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("capacity TEXT").Exec(ctx)
		handleError(tx, err)

		// Add count column to MachineCapability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("count INT").Exec(ctx)
		handleError(tx, err)

		// Add info column to MachineCapability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("info JSONB").Exec(ctx)
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
