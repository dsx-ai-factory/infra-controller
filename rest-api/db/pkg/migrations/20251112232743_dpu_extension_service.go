// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Create table for DpuExtensionService model
		_, err := tx.NewCreateTable().Model((*model.DpuExtensionService)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_site_id_idx")
		handleError(tx, err)

		// Add index for site_id
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_site_id_idx ON dpu_extension_service(site_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_tenant_id_idx")
		handleError(tx, err)

		// Add index for tenant_id (frequently queried)
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_tenant_id_idx ON dpu_extension_service(tenant_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_version_idx")
		handleError(tx, err)

		// Add index for version (frequently queried for hardware identification)
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_version_idx ON dpu_extension_service(version)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_created_idx")
		handleError(tx, err)

		// Add index for created timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_created_idx ON dpu_extension_service(created)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_updated_idx")
		handleError(tx, err)

		// Add index for updated timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_updated_idx ON dpu_extension_service(updated)")
		handleError(tx, err)

		// Create table for DpuExtensionServiceDeployment model
		_, err = tx.NewCreateTable().Model((*model.DpuExtensionServiceDeployment)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_site_id_idx")
		handleError(tx, err)

		// Add index for site_id
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_site_id_idx ON dpu_extension_service_deployment(site_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_tenant_id_idx")
		handleError(tx, err)

		// Add index for tenant_id
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_tenant_id_idx ON dpu_extension_service_deployment(tenant_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_instance_id_idx")
		handleError(tx, err)

		// Add index for instance_id
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_instance_id_idx ON dpu_extension_service_deployment(instance_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_dpu_extension_service_id_idx")
		handleError(tx, err)

		// Add index for dpu_extension_service_id
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_dpu_extension_service_id_idx ON dpu_extension_service_deployment(dpu_extension_service_id)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_version_idx")
		handleError(tx, err)

		// Add index for version
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_version_idx ON dpu_extension_service_deployment(version)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_created_idx")
		handleError(tx, err)

		// Add index for created timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_created_idx ON dpu_extension_service_deployment(created)")
		handleError(tx, err)

		// Drop index if it exists
		_, err = tx.Exec("DROP INDEX IF EXISTS dpu_extension_service_deployment_updated_idx")
		handleError(tx, err)

		// Add index for updated timestamp for default ordering
		_, err = tx.Exec("CREATE INDEX dpu_extension_service_deployment_updated_idx ON dpu_extension_service_deployment(updated)")
		handleError(tx, err)

		// Commit transaction
		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Created 'dpu_extension_service' and 'dpu_extension_service_deployment' tables and indices successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
