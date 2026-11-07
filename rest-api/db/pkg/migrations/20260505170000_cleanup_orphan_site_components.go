// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

// This migration backfills the cleanup that ManageSite.DeleteSiteComponentsFromDB
// now performs when a site is deleted. Prior to that change a number of
// site-scoped tables were left holding rows after their owning site was
// soft-deleted; the statements below soft-delete those orphans so the database
// reflects the state we would have ended up in if the new cleanup had always
// been in place.
//
// infiniband_interface and nvlink_interface were already addressed by
// 20260409120000_cleanup_orphan_infiniband_nvlink_interfaces.go and so are not
// re-processed here.
func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Soft-delete ethernet interfaces with no active instance (mirrors the
		// existing migration for IB/NVLink interfaces).
		_, err := tx.Exec(`
			UPDATE interface ifc
			SET deleted = CURRENT_TIMESTAMP, updated = CURRENT_TIMESTAMP
			WHERE ifc.deleted IS NULL
			AND ifc.instance_id NOT IN (SELECT id FROM instance WHERE deleted IS NULL)`)
		handleError(tx, err)

		// Site-scoped tables with a soft_delete column: mark rows deleted
		// when their site is missing or already soft-deleted.
		softDeleteSiteScopedTables := []struct {
			table string
			alias string
		}{
			{"vpc_prefix", "vp"},
			{"vpc_peering", "vp"},
			{"nvlink_logical_partition", "nvllp"},
			{"ssh_key_group_site_association", "skgsa"},
			{"ssh_key_group_instance_association", "skgia"},
			{"network_security_group", "nsg"},
			{"dpu_extension_service_deployment", "desd"},
			{"operating_system_site_association", "ossa"},
		}
		for _, t := range softDeleteSiteScopedTables {
			stmt := fmt.Sprintf(`
				UPDATE %[1]s %[2]s
				SET deleted = CURRENT_TIMESTAMP, updated = CURRENT_TIMESTAMP
				WHERE %[2]s.deleted IS NULL
				AND %[2]s.site_id NOT IN (SELECT id FROM site WHERE deleted IS NULL)`,
				t.table, t.alias)
			_, err = tx.Exec(stmt)
			handleError(tx, err)
		}

		// Site-scoped tables without a soft_delete column: hard-delete rows
		// whose site is missing or already soft-deleted. The matching DAO
		// Delete methods on these tables are also hard deletes, so this
		// mirrors the runtime cleanup.
		hardDeleteSiteScopedTables := []string{
			"sku",
			"expected_machine",
			"expected_switch",
			"expected_power_shelf",
		}
		for _, table := range hardDeleteSiteScopedTables {
			stmt := fmt.Sprintf(`
				DELETE FROM %[1]s
				WHERE site_id NOT IN (SELECT id FROM site WHERE deleted IS NULL)`,
				table)
			_, err = tx.Exec(stmt)
			handleError(tx, err)
		}

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Soft-deleted orphan site-scoped rows across interface, vpc_prefix, vpc_peering, nvlink_logical_partition, ssh_key_group_site_association, ssh_key_group_instance_association, network_security_group, dpu_extension_service_deployment, and operating_system_site_association; hard-deleted orphan rows from sku, expected_machine, expected_switch, and expected_power_shelf. ")
		return nil
	}, func(_ context.Context, _ *bun.DB) error {
		fmt.Print(" [down migration] No-op (data cleanup cannot be reversed). ")
		return nil
	})
}
