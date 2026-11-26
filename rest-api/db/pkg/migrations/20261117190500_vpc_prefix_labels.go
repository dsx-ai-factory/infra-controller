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
			Model((*model.VpcPrefix)(nil)).
			IfNotExists().
			ColumnExpr("labels JSONB NOT NULL DEFAULT ('{}')").
			Exec(ctx)
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS vpc_prefix_tsv_idx")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX vpc_prefix_tsv_idx ON vpc_prefix USING gin(to_tsvector('english', name || ' ' || status || ' ' || labels::text))")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'labels' column to 'vpc_prefix' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
