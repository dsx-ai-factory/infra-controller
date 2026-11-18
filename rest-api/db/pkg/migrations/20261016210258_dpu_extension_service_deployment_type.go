// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"errors"

	"github.com/uptrace/bun"
)

var errDpuExtensionServiceDeploymentTypeRollback = errors.New("cannot roll back DPU Extension Service deployment type migration because dropping deployment_type would discard whether services are Baseline or DemandDriven")

func init() {
	Migrations.MustRegister(dpuExtensionServiceDeploymentTypeUpMigration, dpuExtensionServiceDeploymentTypeDownMigration)
}

func dpuExtensionServiceDeploymentTypeUpMigration(ctx context.Context, db *bun.DB) error {
	_, err := db.ExecContext(ctx, `ALTER TABLE dpu_extension_service
		ADD COLUMN IF NOT EXISTS deployment_type TEXT NOT NULL DEFAULT 'DemandDriven'`)
	return err
}

func dpuExtensionServiceDeploymentTypeDownMigration(_ context.Context, _ *bun.DB) error {
	return errDpuExtensionServiceDeploymentTypeRollback
}
