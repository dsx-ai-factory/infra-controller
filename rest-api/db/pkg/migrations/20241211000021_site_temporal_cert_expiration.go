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

		// Add agent_cert_expiry column to the Site table
		_, err := tx.NewAddColumn().Model((*model.Site)(nil)).IfNotExists().ColumnExpr("agent_cert_expiry TIMESTAMPTZ DEFAULT NULL").Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print("  [up migration] Added agent_cert_expiry column to Site table")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		// Rollback script to remove cert_expiration column
		_, err := db.NewDropColumn().Model((*model.Site)(nil)).Column("agent_cert_expiry").Exec(ctx)
		if err != nil {
			return fmt.Errorf("failed to drop agent_cert_expiry column: %v", err)
		}

		fmt.Print(" [down migration] Removed agent_cert_expiry column from Site table\n")
		return nil
	})
}
