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

		// Persist VPC-selection intent independently from the resolved VPC prefix.
		_, err := tx.ExecContext(ctx, `ALTER TABLE "interface" ADD COLUMN IF NOT EXISTS vpc_id UUID`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `ALTER TABLE "interface" ADD COLUMN IF NOT EXISTS vpc_ip_family_mode TEXT`)
		handleError(tx, err)

		_, err = tx.ExecContext(ctx, `ALTER TABLE "interface" DROP CONSTRAINT IF EXISTS interface_vpc_id_fkey`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `ALTER TABLE "interface" ADD CONSTRAINT interface_vpc_id_fkey FOREIGN KEY (vpc_id) REFERENCES public.vpc(id)`)
		handleError(tx, err)

		_, err = tx.ExecContext(ctx, `CREATE INDEX IF NOT EXISTS interface_vpc_id_idx ON "interface" (vpc_id)`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added VPC-selection intent columns to 'interface' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			return terr
		}

		if _, err := tx.ExecContext(ctx, `DROP INDEX IF EXISTS interface_vpc_id_idx`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err := tx.ExecContext(ctx, `ALTER TABLE "interface" DROP CONSTRAINT IF EXISTS interface_vpc_id_fkey`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if _, err := tx.ExecContext(ctx, `ALTER TABLE "interface" DROP COLUMN IF EXISTS vpc_ip_family_mode, DROP COLUMN IF EXISTS vpc_id`); err != nil {
			_ = tx.Rollback()
			return err
		}
		if err := tx.Commit(); err != nil {
			return err
		}

		fmt.Print(" [down migration] Dropped VPC-selection intent columns from 'interface' table successfully. ")
		return nil
	})
}
