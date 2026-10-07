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
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.ExecContext(ctx, `ALTER TABLE operating_system DROP COLUMN IF EXISTS is_cloud_init`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Dropped 'is_cloud_init' column from 'operating_system' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.ExecContext(ctx, `ALTER TABLE operating_system ADD COLUMN IF NOT EXISTS is_cloud_init BOOLEAN`)
		handleError(tx, err)

		// Re-derive is_cloud_init from user_data, matching IsCloudInitFromUserData.
		_, err = tx.ExecContext(ctx, `
			UPDATE operating_system
			SET is_cloud_init = (user_data IS NOT NULL AND user_data <> '')
		`)
		handleError(tx, err)

		_, err = tx.ExecContext(ctx, `ALTER TABLE operating_system ALTER COLUMN is_cloud_init SET NOT NULL`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [down migration] Restored 'is_cloud_init' column on 'operating_system' table successfully. ")
		return nil
	})
}
