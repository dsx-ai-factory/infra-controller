// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Core owns Domains and REST keeps no copy, so a Network Segment's Domain
		// has no row in the REST domain table for this key to reference. The
		// domain_id column stays so an existing REST instance can still write it
		// while migrations run. Fresh REST bootstrap creates the table from the
		// current model, without the column or the key.
		_, err := db.ExecContext(ctx, `ALTER TABLE subnet DROP CONSTRAINT IF EXISTS subnet_domain_id_fkey`)
		return err
	}, func(ctx context.Context, db *bun.DB) error {
		return nil
	})
}
