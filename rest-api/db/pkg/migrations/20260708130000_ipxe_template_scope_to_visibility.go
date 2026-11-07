// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

// Renames the ipxe_template `scope` column (and its index) to `visibility`.
// The concept describes a template's visibility (Public/Internal); `scope` was
// confusing next to the operating_system `ipxe_os_scope` column, which is a
// distinct concept (Local/Global/Limited).
//
// This migration is guarded so it is idempotent regardless of starting state:
// environments that already applied the original ipxe_template migration have a
// `scope` column and get renamed; fresh environments create the table straight
// from the (already-renamed) bun model, so the rename is skipped.
func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.Exec(`
			DO $$
			BEGIN
				IF EXISTS (
					SELECT 1 FROM information_schema.columns
					WHERE table_name = 'ipxe_template' AND column_name = 'scope'
				) THEN
					ALTER TABLE ipxe_template RENAME COLUMN scope TO visibility;
				END IF;
				IF EXISTS (SELECT 1 FROM pg_class WHERE relname = 'ipxe_template_scope_idx') THEN
					ALTER INDEX ipxe_template_scope_idx RENAME TO ipxe_template_visibility_idx;
				END IF;
			END $$;
		`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Renamed ipxe_template.scope to ipxe_template.visibility. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.Exec(`
			DO $$
			BEGIN
				IF EXISTS (SELECT 1 FROM pg_class WHERE relname = 'ipxe_template_visibility_idx') THEN
					ALTER INDEX ipxe_template_visibility_idx RENAME TO ipxe_template_scope_idx;
				END IF;
				IF EXISTS (
					SELECT 1 FROM information_schema.columns
					WHERE table_name = 'ipxe_template' AND column_name = 'visibility'
				) THEN
					ALTER TABLE ipxe_template RENAME COLUMN visibility TO scope;
				END IF;
			END $$;
		`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [down migration] Renamed ipxe_template.visibility back to ipxe_template.scope. ")
		return nil
	})
}
