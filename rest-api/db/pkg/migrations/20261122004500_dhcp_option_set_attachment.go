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

		// The initial migration creates these tables from the current models, so a fresh
		// database already has the column; existing databases get it here.
		// Attach DHCP Option Sets to VPCs
		_, err := tx.NewAddColumn().Model((*model.Vpc)(nil)).IfNotExists().ColumnExpr("dhcp_option_set_id uuid REFERENCES dhcp_option_set(id)").Exec(ctx)
		handleError(tx, err)

		// Attach DHCP Option Sets to Instances
		_, err = tx.NewAddColumn().Model((*model.Instance)(nil)).IfNotExists().ColumnExpr("dhcp_option_set_id uuid REFERENCES dhcp_option_set(id)").Exec(ctx)
		handleError(tx, err)

		// Let Instances boot from their DHCP Option Set's boot URL
		_, err = tx.NewAddColumn().Model((*model.Instance)(nil)).IfNotExists().ColumnExpr("boot_from_dhcp_option_set BOOLEAN NOT NULL DEFAULT false").Exec(ctx)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added dhcp_option_set_id column to vpc and instance tables, and boot_from_dhcp_option_set column to instance table. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
