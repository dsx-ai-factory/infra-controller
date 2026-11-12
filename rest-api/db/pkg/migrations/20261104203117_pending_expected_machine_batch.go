// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Keep reservations private and retain referenced REST SKU records until creation is resolved.
		return db.RunInTx(ctx, nil, func(ctx context.Context, tx bun.Tx) error {
			_, err := tx.ExecContext(ctx, `CREATE TABLE pending_expected_machine (
			id UUID PRIMARY KEY,
			workflow_id TEXT NOT NULL,
			run_id TEXT,
			site_id UUID NOT NULL,
			sku_id TEXT REFERENCES sku(id),
			position INTEGER NOT NULL,
			input JSONB NOT NULL,
			created TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
			last_observed TIMESTAMPTZ
		);
		CREATE UNIQUE INDEX pending_expected_machine_site_workflow_position_idx
			ON pending_expected_machine(site_id, workflow_id, position)`)
			return err
		})
	}, func(ctx context.Context, db *bun.DB) error { return nil })
}
