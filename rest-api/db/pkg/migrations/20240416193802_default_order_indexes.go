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
			{index: "allocation_created_idx", table: "public.allocation", column: "created"},
			{index: "allocation_constraint_created_idx", table: "public.allocation_constraint", column: "created"},
			{index: "infiniband_interface_created_idx", table: "public.infiniband_interface", column: "created"},
			{index: "infiniband_partition_created_idx", table: "public.infiniband_partition", column: "created"},
			{index: "instance_created_idx", table: "public.instance", column: "created"},
			{index: "instance_type_created_idx", table: "public.instance_type", column: "created"},
			{index: "interface_created_idx", table: "public.interface", column: "created"},
			{index: "ip_block_created_idx", table: "public.ip_block", column: "created"},
			{index: "machine_created_idx", table: "public.machine", column: "created"},
			{index: "machine_capability_created_idx", table: "public.machine_capability", column: "created"},
			{index: "machine_instance_type_created_idx", table: "public.machine_instance_type", column: "created"},
			{index: "machine_interface_created_idx", table: "public.machine_interface", column: "created"},
			{index: "operating_system_created_idx", table: "public.operating_system", column: "created"},
			{index: "site_created_idx", table: "public.site", column: "created"},
			{index: "ssh_key_created_idx", table: "public.ssh_key", column: "created"},
			{index: "ssh_key_association_created_idx", table: "public.ssh_key_association", column: "created"},
			{index: "sshkey_group_created_idx", table: "public.sshkey_group", column: "created"},
			{index: "ssh_key_group_instance_association_created_idx", table: "public.ssh_key_group_instance_association", column: "created"},
			{index: "ssh_key_group_site_association_created_idx", table: "public.ssh_key_group_site_association", column: "created"},
			{index: "status_detail_created_idx", table: "public.status_detail", column: "created"},
			{index: "subnet_created_idx", table: "public.subnet", column: "created"},
			{index: "tenant_account_created_idx", table: "public.tenant_account", column: "created"},
			{index: "tenant_site_created_idx", table: "public.tenant_site", column: "created"},
			{index: "vpc_created_idx", table: "public.vpc", column: "created"},
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
