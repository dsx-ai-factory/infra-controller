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

		// Create DhcpOptionSet table
		_, err := tx.NewCreateTable().Model((*model.DhcpOptionSet)(nil)).Exec(ctx)
		handleError(tx, err)

		// Add GIN index for dhcp_option_set table
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS dhcp_option_set_gin_idx ON public.dhcp_option_set USING GIN (name gin_trgm_ops, description gin_trgm_ops, status gin_trgm_ops)")
		handleError(tx, err)

		// Indexes backing the default order-by and the common filters
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS dhcp_option_set_created_idx ON public.dhcp_option_set(created)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS dhcp_option_set_site_id_idx ON public.dhcp_option_set(site_id)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS dhcp_option_set_tenant_id_idx ON public.dhcp_option_set(tenant_id)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added DHCP Option Set table. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
