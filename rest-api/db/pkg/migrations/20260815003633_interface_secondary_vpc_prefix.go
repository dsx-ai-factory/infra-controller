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
		tx, err := db.BeginTx(ctx, &sql.TxOptions{})
		if err != nil {
			handlePanic(err, "failed to begin transaction")
		}

		// REST never persisted Core's secondary VPC prefix ID, so there is no
		// value to backfill. `UpdateInstancesInDB` populates this column from the
		// next aligned inventory.
		_, err = tx.ExecContext(ctx, `
			ALTER TABLE "interface"
			ADD COLUMN IF NOT EXISTS secondary_vpc_prefix_id UUID
		`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `
			ALTER TABLE "interface"
			DROP CONSTRAINT IF EXISTS interface_secondary_vpc_prefix_id_fkey
		`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `
			ALTER TABLE "interface"
			ADD CONSTRAINT interface_secondary_vpc_prefix_id_fkey
			FOREIGN KEY (secondary_vpc_prefix_id)
			REFERENCES public.vpc_prefix(id)
		`)
		handleError(tx, err)
		// The original migration used this name for an index on vpc_prefix(id).
		// Replace that redundant index so the name describes the indexed column.
		_, err = tx.ExecContext(ctx, `
			DROP INDEX IF EXISTS interface_vpc_prefix_id_idx
		`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `
			CREATE INDEX interface_vpc_prefix_id_idx
			ON "interface" (vpc_prefix_id)
		`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `
			CREATE INDEX IF NOT EXISTS interface_secondary_vpc_prefix_id_idx
			ON "interface" (secondary_vpc_prefix_id)
		`)
		handleError(tx, err)

		if err = tx.Commit(); err != nil {
			handlePanic(err, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added secondary VPC prefix resolution to 'interface' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		tx, err := db.BeginTx(ctx, &sql.TxOptions{})
		if err != nil {
			return err
		}

		if _, err = tx.ExecContext(ctx, `
			DROP INDEX IF EXISTS interface_secondary_vpc_prefix_id_idx
		`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err = tx.ExecContext(ctx, `
			DROP INDEX IF EXISTS interface_vpc_prefix_id_idx
		`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err = tx.ExecContext(ctx, `
			CREATE INDEX interface_vpc_prefix_id_idx
			ON public.vpc_prefix (id)
		`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err = tx.ExecContext(ctx, `
			ALTER TABLE "interface"
			DROP CONSTRAINT IF EXISTS interface_secondary_vpc_prefix_id_fkey
		`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err = tx.ExecContext(ctx, `
			ALTER TABLE "interface"
			DROP COLUMN IF EXISTS secondary_vpc_prefix_id
		`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if err = tx.Commit(); err != nil {
			return err
		}

		fmt.Print(" [down migration] Dropped secondary VPC prefix resolution from 'interface' table successfully. ")
		return nil
	})
}
