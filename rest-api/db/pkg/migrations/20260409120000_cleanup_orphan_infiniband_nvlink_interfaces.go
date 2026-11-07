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
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Soft-delete InfiniBand interfaces with no active instance (missing FK target or instance soft-deleted).
		_, err := tx.Exec(`
			UPDATE infiniband_interface ibi
			SET deleted = CURRENT_TIMESTAMP, updated = CURRENT_TIMESTAMP
			WHERE ibi.deleted IS NULL
			AND ibi.instance_id NOT IN (SELECT id FROM instance WHERE deleted IS NULL)`)
		handleError(tx, err)

		// Soft-delete NVLink interfaces with no active instance.
		_, err = tx.Exec(`
			UPDATE nvlink_interface nvli
			SET deleted = CURRENT_TIMESTAMP, updated = CURRENT_TIMESTAMP
			WHERE nvli.deleted IS NULL
			AND nvli.instance_id NOT IN (SELECT id FROM instance WHERE deleted IS NULL)`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Soft-deleted orphan infiniband_interface and nvlink_interface rows. ")
		return nil
	}, func(_ context.Context, _ *bun.DB) error {
		fmt.Print(" [down migration] No-op (data cleanup cannot be reversed). ")
		return nil
	})
}
