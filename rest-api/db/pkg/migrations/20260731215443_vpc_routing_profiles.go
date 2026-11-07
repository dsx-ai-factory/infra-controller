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

		_, err := tx.ExecContext(ctx, `ALTER TABLE vpc ADD COLUMN IF NOT EXISTS routing_profile_overrides JSONB`)
		handleError(tx, err)

		_, err = tx.ExecContext(ctx, `ALTER TABLE vpc ADD COLUMN IF NOT EXISTS effective_routing_profile JSONB`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added VPC routing-profile columns successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `ALTER TABLE vpc DROP COLUMN IF EXISTS routing_profile_overrides, DROP COLUMN IF EXISTS effective_routing_profile`)
		if err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped VPC routing-profile columns successfully. ")
		return nil
	})
}
