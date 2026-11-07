// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// The machine table had no index on infrastructure_provider_id even
		// though provider-scoped queries (site machine stats, GPU stats)
		// filter on it. A composite (infrastructure_provider_id, site_id)
		// index also serves the per-site GROUP BY aggregation.
		_, err := tx.Exec("DROP INDEX IF EXISTS machine_infrastructure_provider_id_site_id_idx")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX machine_infrastructure_provider_id_site_id_idx ON public.machine(infrastructure_provider_id, site_id)")
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
