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

		_, err := tx.NewAddColumn().Model((*model.Interface)(nil)).IfNotExists().ColumnExpr("requested_ip_address TEXT").Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'requested_ip_address' column to 'interface' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		if _, err := db.ExecContext(ctx, `ALTER TABLE "interface" DROP COLUMN IF EXISTS requested_ip_address`); err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped 'requested_ip_address' column from 'interface' table successfully. ")
		return nil
	})
}
