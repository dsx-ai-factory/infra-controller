// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

// Tenant Accounts created as part of the service account creation flow have no
// rows in status_detail, so their API status history comes back empty. The flow
// now writes an initial status_detail alongside the account; this migration
// backfills that same row for the active accounts created before the fix.

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.Exec(`
			INSERT INTO status_detail (id, entity_id, status, message, count, created, updated)
			SELECT gen_random_uuid(), ta.id::text, ta.status,
			       'service account enabled, tenant account ready',
			       1, ta.created, ta.created
			FROM tenant_account ta
			WHERE ta.deleted IS NULL
			AND ta.status = 'Ready'
			AND NOT EXISTS (
				SELECT 1 FROM status_detail sd WHERE sd.entity_id = ta.id::text
			)`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Backfilled an initial status_detail row for active Tenant Accounts that had no status history. ")
		return nil
	}, func(_ context.Context, _ *bun.DB) error {
		fmt.Print(" [down migration] No-op (backfilled status_detail rows are indistinguishable from organic history and are not removed). ")
		return nil
	})
}
