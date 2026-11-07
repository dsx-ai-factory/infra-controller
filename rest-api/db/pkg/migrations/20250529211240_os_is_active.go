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

		// Add is_active column
		_, err := tx.NewAddColumn().Model((*model.OperatingSystem)(nil)).IfNotExists().ColumnExpr("is_active BOOLEAN NOT NULL DEFAULT true").Exec(ctx)
		handleError(tx, err)
		// add index
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS operating_system_is_active_idx ON public.operating_system(is_active)")
		handleError(tx, err)

		// Add deactivation_note
		_, err = tx.NewAddColumn().Model((*model.OperatingSystem)(nil)).IfNotExists().ColumnExpr("deactivation_note VARCHAR").Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'is_active' and `deactivation_note` to 'operating_system' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
