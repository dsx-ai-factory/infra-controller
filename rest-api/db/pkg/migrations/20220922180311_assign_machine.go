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

		// Add "is_assigned" column to Machine table
		_, err := tx.NewAddColumn().Model((*model.Machine)(nil)).IfNotExists().ColumnExpr("is_assigned BOOLEAN NOT NULL DEFAULT false").Exec(ctx)
		handleError(tx, err)

		// Add "allocation_constraint_id" column to Instance table
		_, err = tx.NewAddColumn().Model((*model.Instance)(nil)).IfNotExists().ColumnExpr("allocation_constraint_id uuid NOT NULL").Exec(ctx)
		handleError(tx, err)

		// Add allocation contraint foreign key
		// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("ALTER TABLE instance DROP CONSTRAINT IF EXISTS instance_allocation_constraint_id_fkey")
		handleError(tx, err)

		_, err = tx.Exec("ALTER TABLE instance ADD CONSTRAINT instance_allocation_constraint_id_fkey FOREIGN KEY (allocation_constraint_id) REFERENCES public.allocation_constraint(id)")
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
