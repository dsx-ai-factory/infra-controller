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

		// OVS attachment metadata. Nullable because they are set only for OVS
		// attachments; Physical and Virtual rows leave them NULL.
		_, err := tx.ExecContext(ctx, `ALTER TABLE spectrumx_attachment ADD COLUMN IF NOT EXISTS bridge_name TEXT`)
		handleError(tx, err)
		_, err = tx.ExecContext(ctx, `ALTER TABLE spectrumx_attachment ADD COLUMN IF NOT EXISTS ovn_network_name TEXT`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added SpectrumX Attachment OVS metadata columns. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `ALTER TABLE spectrumx_attachment DROP COLUMN IF EXISTS bridge_name; ALTER TABLE spectrumx_attachment DROP COLUMN IF EXISTS ovn_network_name`)
		if err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped SpectrumX Attachment OVS metadata columns. ")
		return nil
	})
}
