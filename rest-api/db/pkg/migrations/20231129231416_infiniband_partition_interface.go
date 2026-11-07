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

		// // Create Fabric table
		// _, err := tx.NewCreateTable().Model((*model.Fabric)(nil)).Exec(ctx)
		// handleError(tx, err)

		// Create InfiniBandPartition table
		_, err := tx.NewCreateTable().Model((*model.InfiniBandPartition)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create InfiniBandInterface table
		_, err = tx.NewCreateTable().Model((*model.InfiniBandInterface)(nil)).Exec(ctx)
		handleError(tx, err)

		// // Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		// _, err = tx.Exec("DROP INDEX IF EXISTS fabric_gin_idx")
		// handleError(tx, err)

		// // Add GIN index for fabric table
		// _, err = tx.Exec("CREATE INDEX fabric_gin_idx ON public.fabric USING GIN (id gin_trgm_ops, status gin_trgm_ops)")
		// handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS infiniband_partition_gin_idx")
		handleError(tx, err)

		// Add GIN index for infiniband_partition table
		_, err = tx.Exec("CREATE INDEX infiniband_partition_gin_idx ON public.infiniband_partition USING GIN (name gin_trgm_ops, description gin_trgm_ops, partition_key gin_trgm_ops, partition_name gin_trgm_ops, status gin_trgm_ops)")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS infiniband_interface_gin_idx")
		handleError(tx, err)

		// Add GIN index for infiniband_interface table
		_, err = tx.Exec("CREATE INDEX infiniband_interface_gin_idx ON public.infiniband_interface USING GIN (device gin_trgm_ops, vendor gin_trgm_ops, physical_guid gin_trgm_ops, guid gin_trgm_ops, status gin_trgm_ops)")
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
