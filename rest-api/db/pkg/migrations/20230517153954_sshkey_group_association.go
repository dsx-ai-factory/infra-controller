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

		// Create SSHKeyGroup table
		_, err := tx.NewCreateTable().Model((*model.SSHKeyGroup)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create SSHKeyGroupSiteAssociation table
		_, err = tx.NewCreateTable().Model((*model.SSHKeyGroupSiteAssociation)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create SSHKeyAssociation table
		// NOTE: This has been moved from migration `20230117133800_ssh_key` to accommodate SSH Key Group foreign key
		_, err = tx.NewCreateTable().Model((*model.SSHKeyAssociation)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		// Drop columns from SSHKey Table
		// This is done using SQLs because, using bun's NewDropColumn() would
		// use the modified model which doesnt have the columns anyway, which causes errors
		_, err = tx.Exec("ALTER TABLE ssh_key DROP COLUMN IF EXISTS is_global")
		handleError(tx, err)

		// Drop columns from SSHKeyAssociation Table
		// This is done using SQLs because, using bun's NewDropColumn() would
		// use the modified model which doesnt have the columns anyway, which causes errors
		_, err = tx.Exec("ALTER TABLE ssh_key_association DROP COLUMN IF EXISTS entity_type")
		handleError(tx, err)

		// Drop columns from SSHKeyAssociation Table
		// This is done using SQLs because, using bun's NewDropColumn() would
		// use the modified model which doesnt have the columns anyway, which causes errors
		_, err = tx.Exec("ALTER TABLE ssh_key_association DROP COLUMN IF EXISTS entity_id")
		handleError(tx, err)

		// Drop columns from SSHKeyAssociation Table
		// This is done using SQLs because, using bun's NewDropColumn() would
		// use the modified model which doesnt have the columns anyway, which causes errors
		_, err = tx.Exec("ALTER TABLE ssh_key_association DROP COLUMN IF EXISTS tenant_id")
		handleError(tx, err)

		// Drop tenant_id foreign key constraint if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("ALTER TABLE ssh_key_association DROP CONSTRAINT IF EXISTS tenant_id_fkey")
		handleError(tx, err)

		// Add sshkey_group_id column to SSHKeyAssociation table
		_, err = tx.NewAddColumn().Model((*model.SSHKeyAssociation)(nil)).IfNotExists().ColumnExpr("sshkey_group_id UUID NOT NULL").Exec(ctx)
		handleError(tx, err)

		// Add sshkey_group_id foreign key constraint for SSHKeyAssociation
		// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("ALTER TABLE ssh_key_association DROP CONSTRAINT IF EXISTS sshkey_group_id_fkey")
		handleError(tx, err)

		// Add sshkey_group_id foreign key constraint
		_, err = tx.Exec("ALTER TABLE ssh_key_association ADD CONSTRAINT sshkey_group_id_fkey FOREIGN KEY (sshkey_group_id) REFERENCES public.sshkey_group(id)")
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
