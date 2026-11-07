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
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Drop if the existing tenant_account_tsv_idx exists
		_, err := tx.Exec("DROP INDEX IF EXISTS tenant_account_tsv_idx")
		handleError(tx, err)

		// Add tsv index for tenant_account table
		_, err = tx.Exec("CREATE INDEX tenant_account_tsv_idx ON tenant_account USING gin(to_tsvector('english', account_number || ' ' || tenant_org))")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Created tenant_account_tsv_idx index successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Drop the index added by the up migration
		_, err := tx.Exec("DROP INDEX IF EXISTS tenant_account_tsv_idx")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [down migration] Dropped tenant_account_tsv_idx index successfully. ")
		return nil
	})
}
