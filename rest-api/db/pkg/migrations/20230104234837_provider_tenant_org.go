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

		// Add org_display_name column to infrastructure_provider table
		_, err := tx.NewAddColumn().Model((*model.InfrastructureProvider)(nil)).IfNotExists().ColumnExpr("org_display_name varchar").Exec(ctx)
		if err != nil {
			handleError(tx, err)
		}

		// Add org_display_name column to tenant table
		_, err = tx.NewAddColumn().Model((*model.Tenant)(nil)).IfNotExists().ColumnExpr("org_display_name varchar").Exec(ctx)
		if err != nil {
			handleError(tx, err)
		}

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
