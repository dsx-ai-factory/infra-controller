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

		_, err := tx.ExecContext(ctx, `ALTER TABLE "interface" ADD COLUMN IF NOT EXISTS inline_routing_profile JSONB`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'inline_routing_profile' column to 'interface' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		if _, err := db.ExecContext(ctx, `ALTER TABLE "interface" DROP COLUMN IF EXISTS inline_routing_profile`); err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped 'inline_routing_profile' column from 'interface' table successfully. ")
		return nil
	})
}
