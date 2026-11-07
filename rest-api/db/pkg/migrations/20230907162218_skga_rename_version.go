// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func sshkeyGroupSiteAssociationVersionRenameUpMigration(ctx context.Context, db *bun.DB) error {
	// Start transaction
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Rename sshkey_group_association table to ssh_key_group_site_association if it exists
	_, err := tx.Exec("ALTER TABLE IF EXISTS sshkey_group_association RENAME TO ssh_key_group_site_association")
	handleError(tx, err)

	// Rename controller_keyset_version to version if controller_keyset_version column exists
	res, err := tx.Exec("SELECT column_name FROM information_schema.columns WHERE table_name = 'ssh_key_group_site_association' AND column_name = 'controller_keyset_version'")
	handleError(tx, err)
	rowsAffected, err := res.RowsAffected()
	handleError(tx, err)
	fmt.Println("rows affected:", rowsAffected)
	if rowsAffected > 0 {
		_, err := tx.Exec("ALTER TABLE ssh_key_group_site_association RENAME COLUMN controller_keyset_version TO version")
		handleError(tx, err)
	}

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(sshkeyGroupSiteAssociationVersionRenameUpMigration, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
