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

		//~~ Create NVLinkLogicalPartition table & indexes ~~//
		// Create NVLinkLogicalPartition table
		_, err := tx.NewCreateTable().Model((*model.NVLinkLogicalPartition)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop existing indexes
		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_logical_partition_site_id_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_logical_partition_tenant_id_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_logical_partition_created_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_logical_partition_updated_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_logical_partition_tsv_idx")
		handleError(tx, err)

		// Create new indexes
		_, err = tx.Exec("CREATE INDEX nvlink_logical_partition_site_id_idx ON nvlink_logical_partition(site_id)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX nvlink_logical_partition_tenant_id_idx ON nvlink_logical_partition(tenant_id)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX nvlink_logical_partition_created_idx ON nvlink_logical_partition(created)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX nvlink_logical_partition_updated_idx ON nvlink_logical_partition(updated)")
		handleError(tx, err)

		// Add text search vector index
		_, err = tx.Exec("CREATE INDEX nvlink_logical_partition_tsv_idx ON nvlink_logical_partition USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		//~~ Create NVLinkInterface table & indexes ~~//
		// Create NVLinkInterface table
		_, err = tx.NewCreateTable().Model((*model.NVLinkInterface)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop existing indexes
		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_interface_site_id_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_interface_instance_id_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_interface_created_idx")
		handleError(tx, err)

		_, err = tx.Exec("DROP INDEX IF EXISTS nvlink_interface_updated_idx")
		handleError(tx, err)

		// Create new indexes
		_, err = tx.Exec("CREATE INDEX nvlink_interface_site_id_idx ON nvlink_interface(site_id)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX nvlink_interface_instance_id_idx ON nvlink_interface(instance_id)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX nvlink_interface_created_idx ON nvlink_interface(created)")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX nvlink_interface_updated_idx ON nvlink_interface(updated)")
		handleError(tx, err)

		// Add text search vector index
		_, err = tx.Exec("CREATE INDEX nvlink_interface_tsv_idx ON nvlink_interface USING gin(to_tsvector('english', device || ' ' || status))")
		handleError(tx, err)

		//~~ Update VPC table to add NVLink column ~~//
		// Add nvlink_logical_partition_id column to vpc table
		_, err = tx.NewAddColumn().Model((*model.Vpc)(nil)).IfNotExists().ColumnExpr("nvlink_logical_partition_id UUID DEFAULT NULL").Exec(ctx)
		handleError(tx, err)

		// Drop foreign key constraint if exists
		_, err = tx.Exec("ALTER TABLE vpc DROP CONSTRAINT IF EXISTS vpc_nvlink_logical_partition_id_fkey")
		handleError(tx, err)

		// Add `vpc_nvlink_logical_partition_id_fkey`foreign key constraint
		_, err = tx.Exec("ALTER TABLE vpc ADD CONSTRAINT vpc_nvlink_logical_partition_id_fkey FOREIGN KEY (nvlink_logical_partition_id) REFERENCES public.nvlink_logical_partition(id)")
		handleError(tx, err)

		// Drop index for nvlink_logical_partition_id column
		_, err = tx.Exec("DROP INDEX IF EXISTS vpc_nvlink_logical_partition_id_idx")
		handleError(tx, err)

		// Add index for nvlink_logical_partition_id column
		_, err = tx.Exec("CREATE INDEX vpc_nvlink_logical_partition_id_idx ON vpc(nvlink_logical_partition_id)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}
		fmt.Print(" [up migration] Created 'NVLinkLogicalPartition' and 'NVLinkInterface' tables and created indices successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] Dropped NVLinkLogicalPartition and NVLinkInterface tables successfully. ")
		return nil
	})
}
