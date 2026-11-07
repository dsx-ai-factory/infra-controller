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

		// Add labels column to instance_type table
		_, err := tx.NewAddColumn().Model((*model.InstanceType)(nil)).IfNotExists().ColumnExpr("labels JSONB NOT NULL DEFAULT ('{}')").Exec(ctx)
		handleError(tx, err)

		// Drop if the existing instance_type_tsv_idx exists
		_, err = tx.Exec("DROP INDEX IF EXISTS instance_type_tsv_idx")
		handleError(tx, err)

		// Add tsv index which includes labels for instance_type table
		_, err = tx.Exec("CREATE INDEX instance_type_tsv_idx ON instance_type USING gin(to_tsvector('english', name || ' ' || display_name || ' ' || description || ' ' || labels::text || ' ' || status))")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'labels' column to 'instance_type' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
