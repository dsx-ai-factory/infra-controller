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

		// Add index column to machine_capability table
		_, err := tx.NewAddColumn().Model((*model.MachineCapability)(nil)).IfNotExists().ColumnExpr("index int").Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Added new index column to machine_capability table")

		// Add version column to instance_type table
		_, err = tx.NewAddColumn().Model((*model.InstanceType)(nil)).IfNotExists().ColumnExpr("version varchar").Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Added new version column to instance_type table")

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
