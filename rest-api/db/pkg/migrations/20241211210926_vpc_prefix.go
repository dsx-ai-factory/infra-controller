// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"fmt"

	"database/sql"

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

		// create audit entry table
		_, err := tx.NewCreateTable().Model((*model.VpcPrefix)(nil)).Exec(ctx)
		handleError(tx, err)

		// VpcPrefix index
		// Drop if the vpc_prefix name/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS vpc_prefix_tsv_idx")
		handleError(tx, err)

		// Add tsv index
		_, err = tx.Exec("CREATE INDEX vpc_prefix_tsv_idx ON vpc_prefix USING gin(to_tsvector('english', name || ' ' || status))")
		handleError(tx, err)
		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
