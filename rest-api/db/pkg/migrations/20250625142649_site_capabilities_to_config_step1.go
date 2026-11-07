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

		/*
		  Step 1 in the migration process is to create the new column and copy from new to old.
		  Step 2 will be to later update the model and DB to remove the old column AFTER all
		  dependent code is updated to begin using the new column.
		*/

		// Add config column
		_, err := tx.NewAddColumn().Model((*model.Site)(nil)).IfNotExists().ColumnExpr("config jsonb DEFAULT '{}'::jsonb").Exec(ctx)
		handleError(tx, err)

		// Copy all capabilities to config column
		_, err = tx.Exec("UPDATE site SET config=capabilities WHERE deleted IS NULL")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'config' to 'site' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
