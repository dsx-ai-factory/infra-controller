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
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Nullable because the Site Agent reports this on its Site Config inventory, so a Site
		// stays NULL until its first report. Readers fall back to the built-in interval rather
		// than treating a missing value as zero.
		_, err := tx.NewAddColumn().
			Model((*model.Site)(nil)).
			IfNotExists().
			ColumnExpr("inventory_interval_seconds INTEGER").
			Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'inventory_interval_seconds' column to 'site' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		// Safe to drop. The Site Agent re-reports the interval on its next inventory tick, so
		// nothing here has to be reconstructed by hand.
		_, err := db.ExecContext(ctx, `ALTER TABLE site DROP COLUMN IF EXISTS inventory_interval_seconds`)
		if err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped 'inventory_interval_seconds' column from 'site' table successfully. ")
		return nil
	})
}
