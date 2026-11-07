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

		// Add is_in_maintenance column to Machine table
		_, err := tx.NewAddColumn().Model((*model.Machine)(nil)).IfNotExists().ColumnExpr("is_in_maintenance BOOLEAN NOT NULL DEFAULT false").Exec(ctx)
		handleError(tx, err)

		// Add maintenance_message column to Machine table
		_, err = tx.NewAddColumn().Model((*model.Machine)(nil)).IfNotExists().ColumnExpr("maintenance_message VARCHAR").Exec(ctx)
		handleError(tx, err)

		// Add is_network_degraded column to Machine table
		_, err = tx.NewAddColumn().Model((*model.Machine)(nil)).IfNotExists().ColumnExpr("is_network_degraded BOOLEAN NOT NULL DEFAULT false").Exec(ctx)
		handleError(tx, err)

		// Add network_health_message column to Machine table
		_, err = tx.NewAddColumn().Model((*model.Machine)(nil)).IfNotExists().ColumnExpr("network_health_message VARCHAR").Exec(ctx)
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
