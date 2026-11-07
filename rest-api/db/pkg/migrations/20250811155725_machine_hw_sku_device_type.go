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

		// Add hw_sku_device_type column to machine table
		_, err := tx.NewAddColumn().Model((*model.Machine)(nil)).IfNotExists().ColumnExpr("hw_sku_device_type TEXT").Exec(ctx)
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS machine_hw_sku_device_type_indx")
		handleError(tx, err)

		// Add index for hw_sku_device_type column in machine table
		_, err = tx.Exec("CREATE INDEX machine_hw_sku_device_type_indx ON public.machine(hw_sku_device_type)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'hw_sku_device_type' column to 'machine' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Drop index first
		_, err := tx.Exec("DROP INDEX IF EXISTS machine_hw_sku_device_type_indx")
		handleError(tx, err)

		// Remove hw_sku_device_type column from machine table
		_, err = tx.NewDropColumn().Model((*model.Machine)(nil)).Column("hw_sku_device_type").Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [down migration] Removed 'hw_sku_device_type' column from 'machine' table successfully. ")
		return nil
	})
}
