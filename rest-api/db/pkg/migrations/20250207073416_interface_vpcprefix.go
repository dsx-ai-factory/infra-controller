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

		// Add vpc_prefix_id column to the interface table
		_, err := tx.NewAddColumn().Model((*model.Interface)(nil)).IfNotExists().ColumnExpr("vpc_prefix_id UUID").Exec(ctx)
		handleError(tx, err)

		// Drop 'NOT NULL' so subnet_id field can be nullable
		_, err = tx.Exec("ALTER TABLE interface ALTER COLUMN subnet_id DROP NOT NULL")
		handleError(tx, err)

		// Add vpc_prefix foreign key for interface
		// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("ALTER TABLE interface DROP CONSTRAINT IF EXISTS interface_vpc_prefix_id_fkey")
		handleError(tx, err)

		_, err = tx.Exec("ALTER TABLE interface ADD CONSTRAINT interface_vpc_prefix_id_fkey FOREIGN KEY (vpc_prefix_id) REFERENCES public.vpc_prefix(id)")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS interface_vpc_prefix_id_idx")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX interface_vpc_prefix_id_idx ON public.vpc_prefix(id)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'vpc_prefix_id' column to 'interface' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
