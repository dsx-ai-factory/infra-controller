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

		// VPC index
		// Drop if the vpc name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err := tx.Exec("DROP INDEX IF EXISTS vpc_tsv_idx")
		handleError(tx, err)

		// Add tsv index for vpc table
		_, err = tx.Exec("CREATE INDEX vpc_tsv_idx ON vpc USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// Allocation index
		// Drop if the allocation name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS allocation_tsv_idx")
		handleError(tx, err)

		// Add tsv index for allocation table
		_, err = tx.Exec("CREATE INDEX allocation_tsv_idx ON allocation USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// Instance index
		// Drop if the instance name/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS instance_tsv_idx")
		handleError(tx, err)

		// Add tsv index for instance table
		_, err = tx.Exec("CREATE INDEX instance_tsv_idx ON instance USING gin(to_tsvector('english', name || ' ' || status))")
		handleError(tx, err)

		// InstanceType index
		// Drop if the instancetype name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS instancetype_tsv_idx")
		handleError(tx, err)

		// Add tsv index for instancetype table
		_, err = tx.Exec("CREATE INDEX instancetype_tsv_idx ON instance_type USING gin(to_tsvector('english', name || ' ' || display_name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// IPBlock index
		// Drop if the ipblock name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS ipblock_tsv_idx")
		handleError(tx, err)

		// Add tsv index for ipblock table
		_, err = tx.Exec("CREATE INDEX ipblock_tsv_idx ON ip_block USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// OperatingSystem index
		// Drop if the operatingsystem name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS os_tsv_idx")
		handleError(tx, err)

		// Add tsv index for operatingsystem table
		_, err = tx.Exec("CREATE INDEX os_tsv_idx ON operating_system USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// Site index
		// Drop if the site name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS site_tsv_idx")
		handleError(tx, err)

		// Add tsv index for site table
		_, err = tx.Exec("CREATE INDEX site_tsv_idx ON site USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// Subnet index
		// Drop if the subnet name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS subnet_tsv_idx")
		handleError(tx, err)

		// Add tsv index for subnet table
		_, err = tx.Exec("CREATE INDEX subnet_tsv_idx ON subnet USING gin(to_tsvector('english', name || ' ' || description || ' ' || status))")
		handleError(tx, err)

		// SSHKey index
		// Drop if the sshkey name/description/status index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS sshkey_tsv_idx")
		handleError(tx, err)

		// Add tsv index for sshkey table
		_, err = tx.Exec("CREATE INDEX sshkey_tsv_idx ON ssh_key USING gin(to_tsvector('english', name))")
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
