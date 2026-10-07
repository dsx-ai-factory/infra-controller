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

		_, err := tx.ExecContext(ctx, `ALTER TABLE vpc ADD COLUMN IF NOT EXISTS power_resource_group TEXT`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `ALTER TABLE instance ADD COLUMN IF NOT EXISTS power_profile TEXT`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added power provisioning fields successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `ALTER TABLE vpc DROP COLUMN IF EXISTS power_resource_group; ALTER TABLE instance DROP COLUMN IF EXISTS power_profile`)
		if err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped power provisioning fields successfully. ")
		return nil
	})
}
