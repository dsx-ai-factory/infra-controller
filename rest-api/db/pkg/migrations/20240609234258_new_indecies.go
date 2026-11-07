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
		indexes := []struct {
			index  string
			table  string
			column string
		}{
			// User table: Already indexed on auxiliary_id
			{index: "user_startfleet_id_idx", table: "public.user", column: "starfleet_id"},

			// machine_interface table: Already indexed on created
			{index: "machine_interface_machine_id_idx", table: "public.machine_interface", column: "machine_id"},

			// site table: Already indexed on status
			{index: "site_org_idx", table: "public.site", column: "org"},
			{index: "site_infrastructure_provider_id_idx", table: "public.site", column: "infrastructure_provider_id"},

			// infiniband_partition table: Already indexed on created
			{index: "infiniband_partition_site_id_idx", table: "public.infiniband_partition", column: "site_id"},
			{index: "infiniband_partition_tenant_id_idx", table: "public.infiniband_partition", column: "tenant_id"},

			// instance table: Already indexed on created
			{index: "instance_tenant_id_idx", table: "public.instance", column: "tenant_id"},
			{index: "instance_site_id_idx", table: "public.instance", column: "site_id"},

			// instance_type table
			{index: "instance_type_site_id_idx", table: "public.instance_type", column: "site_id"},
			{index: "instance_type_infrastructure_provider_id_idx", table: "public.instance_type", column: "infrastructure_provider_id"},

			// ip_block table: Already indexed on status
			{index: "ip_block_site_id_idx", table: "public.ip_block", column: "site_id"},
			{index: "ip_block_tenant_id_idx", table: "public.ip_block", column: "tenant_id"},

			// ssh_key table: Already indexed on created
			{index: "ssh_key_tenant_id_idx", table: "public.ssh_key", column: "tenant_id"},

			// ssh_key_association table: Already indexed on created
			{index: "ssh_key_association_ssh_key_group_id_idx", table: "public.ssh_key_association", column: "sshkey_group_id"},
			{index: "ssh_key_association_ssh_key_id_idx", table: "public.ssh_key_association", column: "ssh_key_id"},

			// ssh_key_group_site_association table: Already indexed on created
			{index: "ssh_key_group_site_association_ssh_key_group_id_idx", table: "public.ssh_key_group_site_association", column: "sshkey_group_id"},
		}

		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		for _, idx := range indexes {
			// drop index (won't occur/harmless in dev/stage/prod but helps with test)
			_, err := tx.Exec(fmt.Sprintf("DROP INDEX IF EXISTS %s", idx.index))
			handleError(tx, err)

			// add index
			_, err = tx.Exec(fmt.Sprintf("CREATE INDEX %s ON %s(%s)", idx.index, idx.table, idx.column))
			handleError(tx, err)
		}

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
