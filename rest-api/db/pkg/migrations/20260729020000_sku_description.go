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

		_, err := tx.NewAddColumn().
			Model((*model.SKU)(nil)).
			IfNotExists().
			ColumnExpr("description TEXT NOT NULL DEFAULT ''").
			Exec(ctx)
		handleError(tx, err)

		_, err = tx.NewAddColumn().
			Model((*model.SKU)(nil)).
			IfNotExists().
			ColumnExpr("schema_version INTEGER NOT NULL DEFAULT 0").
			Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'description' and 'schema_version' columns to 'sku' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `ALTER TABLE sku DROP COLUMN IF EXISTS description, DROP COLUMN IF EXISTS schema_version`)
		if err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped 'description' and 'schema_version' columns from 'sku' table successfully. ")
		return nil
	})
}
