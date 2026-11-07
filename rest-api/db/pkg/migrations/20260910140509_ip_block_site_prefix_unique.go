// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"errors"
	"fmt"

	"github.com/uptrace/bun"
)

var errIPBlockSitePrefixIdentityRollback = errors.New(
	"cannot roll back unique Core SitePrefix links for REST IP Blocks",
)

func ipBlockSitePrefixUniqueUpMigration(ctx context.Context, db *bun.DB) error {
	// Bun may replay this callback after a crash before it records the
	// migration, so index creation must tolerate an identical retry.
	// Soft-deleted rows keep their Core identity, so the index deliberately
	// excludes only NULL links rather than deleted records.
	_, err := db.ExecContext(ctx, `
		CREATE UNIQUE INDEX IF NOT EXISTS ip_block_site_prefix_id_key
		ON ip_block (site_prefix_id)
		WHERE site_prefix_id IS NOT NULL
	`)
	if err != nil {
		return err
	}

	fmt.Print(" [up migration] Ensured unique Core SitePrefix links on 'ip_block'. ")
	return nil
}

func init() {
	Migrations.MustRegister(ipBlockSitePrefixUniqueUpMigration, func(_ context.Context, _ *bun.DB) error {
		// Keep the migration applied so a later deployment cannot allow two REST
		// rows to claim the same Core SitePrefix ID.
		return errIPBlockSitePrefixIdentityRollback
	})
}
