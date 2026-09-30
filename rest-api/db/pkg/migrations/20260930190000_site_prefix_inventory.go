// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"errors"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Bun can replay a callback before recording its completion. Absence
		// means no inventory received and no authoritative cutover.
		_, err := db.ExecContext(ctx, `ALTER TABLE site
			ADD COLUMN IF NOT EXISTS site_prefix_inventory_progress JSONB,
			ADD COLUMN IF NOT EXISTS site_prefix_inventory_observed_at TIMESTAMPTZ`)
		return err
	}, func(_ context.Context, _ *bun.DB) error {
		return errors.New("cannot discard SitePrefix inventory identity and cutover state")
	})
}
