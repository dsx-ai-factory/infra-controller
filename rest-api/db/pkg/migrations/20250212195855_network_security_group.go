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

		// Create Security Group table
		_, err := tx.NewCreateTable().Model((*model.NetworkSecurityGroup)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Add network_security_group table successfully.")

		// Add nsg id column to instance table
		_, err = tx.NewAddColumn().Model((*model.Instance)(nil)).IfNotExists().ColumnExpr("network_security_group_id varchar(64)").Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Added network_security_group_id column to instance table successfully.")

		// Add network_security_group_propagation_details column to instance table
		_, err = tx.NewAddColumn().Model((*model.Instance)(nil)).IfNotExists().ColumnExpr("network_security_group_propagation_details jsonb").Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Added network_security_group_propagation_details column to instance table successfully.")

		// Add nsg id column to vpc table
		_, err = tx.NewAddColumn().Model((*model.Vpc)(nil)).IfNotExists().ColumnExpr("network_security_group_id varchar(64)").Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Added network_security_group_id column to vpc table successfully.")

		// Add network_security_group_propagation_details column to vpc table
		_, err = tx.NewAddColumn().Model((*model.Vpc)(nil)).IfNotExists().ColumnExpr("network_security_group_propagation_details jsonb").Exec(ctx)
		handleError(tx, err)
		fmt.Print(" [up migration] Added network_security_group_propagation_details column to vpc table successfully.")

		// Add tsv index for network_security_group table
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS nsg_tsv_idx ON network_security_group USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)
		fmt.Print(" [up migration] Created index nsg_tsv_idx on network_security_group table successfully.")

		// add index
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS network_security_group_created_idx ON network_security_group(created)")
		handleError(tx, err)
		fmt.Print(" [up migration] Created index network_security_group_created_idx on network_security_group table successfully.")

		// Add GIN-like index
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS network_security_group_gin_idx ON public.network_security_group USING GIN (name gin_trgm_ops, description gin_trgm_ops, status gin_trgm_ops)")
		handleError(tx, err)
		fmt.Print(" [up migration] Created index network_security_group_gin_idx on network_security_group table successfully.")

		// Add status index for security_group table
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS network_security_group_status_idx ON public.network_security_group(status) WHERE deleted IS NULL")
		handleError(tx, err)
		fmt.Print(" [up migration] Created index network_security_group_status_idx on network_security_group table successfully.")

		// Clean up old tables
		_, err = tx.Exec("DROP TABLE IF EXISTS security_group_association")
		handleError(tx, err)
		fmt.Print(" [up migration] Removed security_group_association table successfully.")

		_, err = tx.Exec("DROP TABLE IF EXISTS security_policy")
		handleError(tx, err)
		fmt.Print(" [up migration] Removed security_policy table successfully.")

		_, err = tx.Exec("DROP TABLE IF EXISTS security_group")
		handleError(tx, err)
		fmt.Print(" [up migration] Removed security_group table successfully.")

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
