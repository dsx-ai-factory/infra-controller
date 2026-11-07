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
			// allocation table: Already indexed on created, status
			{index: "allocation_site_id_idx", table: "public.allocation", column: "site_id"},
			{index: "allocation_tenant_id_idx", table: "public.allocation", column: "tenant_id"},
			{index: "allocation_infrastructure_provider_id_idx", table: "public.allocation", column: "infrastructure_provider_id"},

			// subnet table: Already indexed on created
			{index: "subnet_site_id_idx", table: "public.subnet", column: "site_id"},
			{index: "subnet_tenant_id_idx", table: "public.subnet", column: "tenant_id"},

			// instance_type table: Already indexed on created and status
			{index: "instance_type_site_id_idx", table: "public.instance_type", column: "site_id"},
			{index: "instance_type_infrastructure_provider_id_idx", table: "public.instance_type", column: "infrastructure_provider_id"},

			// vpc table: Already indexed on created
			{index: "vpc_site_id_idx", table: "public.vpc", column: "site_id"},

			// table allocation_constraint: Already indexed on created
			{index: "allocation_constraint_resource_type_id_idx", table: "public.allocation_constraint", column: "resource_type_id"},
			{index: "allocation_constraint_allocation_id_idx", table: "public.allocation_constraint", column: "allocation_id"},

			// table status_detail : Already indexed on created
			{index: "status_detail_entity_id_idx", table: "public.status_detail", column: "entity_id"},

			// tenant table: Already indexed on created
			{index: "tenant_org_idx", table: "public.tenant", column: "org"},

			// tenant_account table: Already indexed on created
			{index: "tenant_account_tenant_id_idx", table: "public.tenant_account", column: "tenant_id"},
			{index: "tenant_account_account_number_idx", table: "public.tenant_account", column: "account_number"},

			// tenant_site table: Already indexed on created
			{index: "tenant_site_tenant_id_idx", table: "public.tenant_site", column: "tenant_id"},
			{index: "tenant_site_site_id_idx", table: "public.tenant_site", column: "site_id"},

			// interface table: Already indexed on created
			{index: "interface_subnet_id_idx", table: "public.interface", column: "subnet_id"},
			{index: "interface_instance_id_idx", table: "public.interface", column: "instance_id"},

			// infrastructure_provider table: Already indexed on created
			{index: "infrastructure_provider_org_idx", table: "public.infrastructure_provider", column: "org"},
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
