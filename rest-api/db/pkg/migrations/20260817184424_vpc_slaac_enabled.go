// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/uptrace/bun"
)

var errVpcSlaacEnabledRollback = errors.New(
	"cannot roll back VPC SLAAC migration because doing so would discard persisted SLAAC policy",
)

func init() {
	Migrations.MustRegister(vpcSlaacEnabledUpMigration, vpcSlaacEnabledDownMigration)
}

func vpcSlaacEnabledUpMigration(ctx context.Context, db *bun.DB) error {
	// Fresh databases create vpc from the current Bun model, while upgraded
	// databases reach this migration without the column. Converge both states.
	err := db.RunInTx(ctx, &sql.TxOptions{}, func(ctx context.Context, tx bun.Tx) error {
		_, err := tx.ExecContext(ctx, `ALTER TABLE vpc ADD COLUMN IF NOT EXISTS slaac_enabled BOOLEAN NOT NULL DEFAULT FALSE`)
		if err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx, `ALTER TABLE vpc ALTER COLUMN slaac_enabled SET DEFAULT FALSE`)
		return err
	})
	if err != nil {
		return err
	}
	fmt.Print(" [up migration] Ensured 'slaac_enabled' exists on 'vpc' table. ")
	return nil
}

func vpcSlaacEnabledDownMigration(_ context.Context, _ *bun.DB) error {
	// Older binaries tolerate this additive column. Refuse schema rollback because
	// dropping it would discard VPC policy that cannot be reconstructed.
	return errVpcSlaacEnabledRollback
}
