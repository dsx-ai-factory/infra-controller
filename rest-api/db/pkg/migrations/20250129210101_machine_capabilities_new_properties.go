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

		// Add hardware_revision column to machine_capability table
		_, err := tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("hardware_revision character varying").Exec(ctx)
		handleError(tx, err)

		// Add cores column to machine_capability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("cores integer").Exec(ctx)
		handleError(tx, err)

		// Add threads column to machine_capability table
		_, err = tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("threads integer").Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'hardware_revision', 'cores', and 'threads' columns to 'machine_capability' table successfully.")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
