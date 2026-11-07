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
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err := tx.Exec("DROP INDEX IF EXISTS allocation_status_idx")
		handleError(tx, err)

		// Add status index for allocation table
		_, err = tx.Exec("CREATE INDEX allocation_status_idx ON public.allocation(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS domain_status_idx")
		handleError(tx, err)

		// Add status index for domain table
		_, err = tx.Exec("CREATE INDEX domain_status_idx ON public.domain(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS ip_block_status_idx")
		handleError(tx, err)

		// Add status index for ip_block table
		_, err = tx.Exec("CREATE INDEX ip_block_status_idx ON public.ip_block(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS instance_type_status_idx")
		handleError(tx, err)

		// Add status index for instance_type table
		_, err = tx.Exec("CREATE INDEX instance_type_status_idx ON public.instance_type(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS interface_status_idx")
		handleError(tx, err)

		// Add status index for interface (formerly instance_subnet) table
		_, err = tx.Exec("CREATE INDEX interface_status_idx ON public.interface(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS instance_status_idx")
		handleError(tx, err)

		// Add status index for instance table
		_, err = tx.Exec("CREATE INDEX instance_status_idx ON public.instance(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS machine_status_idx")
		handleError(tx, err)

		// Add status index for machine table
		_, err = tx.Exec("CREATE INDEX machine_status_idx ON public.machine(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS operating_system_status_idx")
		handleError(tx, err)

		// Add status index for operating_system table
		_, err = tx.Exec("CREATE INDEX operating_system_status_idx ON public.operating_system(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS site_status_idx")
		handleError(tx, err)

		// Add status index for site table
		_, err = tx.Exec("CREATE INDEX site_status_idx ON public.site(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS subnet_status_idx")
		handleError(tx, err)

		// Add status index for subnet table
		_, err = tx.Exec("CREATE INDEX subnet_status_idx ON public.subnet(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS tenant_account_status_idx")
		handleError(tx, err)

		// Add status index for tenant_account table
		_, err = tx.Exec("CREATE INDEX tenant_account_status_idx ON public.tenant_account(status) WHERE deleted IS NULL")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS vpc_status_idx")
		handleError(tx, err)

		// Add status index for vpc table
		_, err = tx.Exec("CREATE INDEX vpc_status_idx ON public.vpc(status) WHERE deleted IS NULL")
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
