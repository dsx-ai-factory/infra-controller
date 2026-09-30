// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"fmt"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(domainLifecycleUpMigration, domainLifecycleDownMigration)
}

func domainLifecycleUpMigration(ctx context.Context, db *bun.DB) error {
	// Only REST-owned reservations participate. Existing unowned inventory rows
	// remain untouched; a name never establishes Core ownership.
	_, err := db.ExecContext(ctx, `
		CREATE UNIQUE INDEX domain_owned_name_idx
		ON domain (tenant_id, site_id, lower(rtrim(hostname, '.')))
		WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
		AND controller_domain_id IS NOT NULL
	`)
	if err != nil {
		return err
	}
	fmt.Print(" [up migration] Added owned Domain reservation uniqueness. ")
	return nil
}

func domainLifecycleDownMigration(_ context.Context, _ *bun.DB) error {
	// Preserving the index prevents old processes from creating duplicate
	// reservations during a rollback. A later deliberate migration can drop it.
	fmt.Print(" [down migration] No action taken")
	return nil
}
