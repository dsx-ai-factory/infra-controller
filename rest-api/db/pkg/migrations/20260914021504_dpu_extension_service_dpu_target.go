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

var errDpuExtensionServiceDpuTargetRollback = errors.New(
	"cannot roll back DPU Extension Service DPU target migration because doing so would discard placement policy",
)

func init() {
	Migrations.MustRegister(dpuExtensionServiceDpuTargetUpMigration, dpuExtensionServiceDpuTargetDownMigration)
}

func dpuExtensionServiceDpuTargetUpMigration(ctx context.Context, db *bun.DB) error {
	err := db.RunInTx(ctx, &sql.TxOptions{}, func(ctx context.Context, tx bun.Tx) error {
		if _, err := tx.ExecContext(ctx, `ALTER TABLE dpu_extension_service ADD COLUMN IF NOT EXISTS dpu_target TEXT`); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `UPDATE dpu_extension_service SET dpu_target = 'AllActive' WHERE service_type = 'DpfHelmChart' AND dpu_target IS NULL`); err != nil {
			return err
		}
		return nil
	})
	if err != nil {
		return err
	}
	fmt.Print(" [up migration] Ensured 'dpu_target' exists on 'dpu_extension_service' table. ")
	return nil
}

func dpuExtensionServiceDpuTargetDownMigration(_ context.Context, _ *bun.DB) error {
	return errDpuExtensionServiceDpuTargetRollback
}
