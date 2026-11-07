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

		_, err := tx.NewCreateTable().Model((*model.VpcPeering)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create VpcPeering index on all vpc1_id, vpc2_id, and site_id
		_, err = tx.Exec("DROP INDEX IF EXISTS idx_vpc_peering_vpc1")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS idx_vpc_peering_vpc1 ON vpc_peering(vpc1_id)")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS idx_vpc_peering_vpc2")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS idx_vpc_peering_vpc2 ON vpc_peering(vpc2_id)")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS idx_vpc_peering_site_id")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS idx_vpc_peering_site_id ON vpc_peering(site_id)")
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
