// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

type legacyTenantSiteConfigRow struct {
	bun.BaseModel `bun:"table:tenant_site,alias:ts"`
	ID            uuid.UUID              `bun:"id,pk,type:uuid"`
	Config        map[string]interface{} `bun:"config,type:jsonb"`
}

type legacyTenantConfigRow struct {
	bun.BaseModel `bun:"table:tenant,alias:t"`
	ID            uuid.UUID              `bun:"id,pk,type:uuid"`
	Config        map[string]interface{} `bun:"config,type:jsonb"`
}

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.NewAddColumn().Model((*model.TenantAccount)(nil)).IfNotExists().
			ColumnExpr("config JSONB NOT NULL DEFAULT '{}'::jsonb").Exec(ctx)
		handleError(tx, err)

		// Backfill tenant_account.config from each Tenant's legacy tenant.config.
		// Only Ready accounts inherit the flag; Tenants without an explicit
		// targetedInstanceCreation value leave their accounts at the '{}' default.
		tenantRows := []legacyTenantConfigRow{}
		err = tx.NewSelect().Model(&tenantRows).Scan(ctx)
		handleError(tx, err)

		for _, row := range tenantRows {
			rawVal, ok := row.Config["targetedInstanceCreation"]
			if !ok {
				continue
			}
			enabled, ok := rawVal.(bool)
			if !ok {
				continue
			}

			_, err = tx.NewUpdate().
				Model((*model.TenantAccount)(nil)).
				Set("config = jsonb_set(COALESCE(config, '{}'::jsonb), '{targetedInstanceCreation}', to_jsonb(?::boolean), true)", enabled).
				Where("tenant_id = ?", row.ID).
				Where("status = ?", model.TenantAccountStatusReady).
				Exec(ctx)
			handleError(tx, err)
		}

		// Normalize tenant_site.config to the current TenantSiteConfig shape.
		legacyRows := []legacyTenantSiteConfigRow{}
		err = tx.NewSelect().Model(&legacyRows).Scan(ctx)
		handleError(tx, err)

		for _, row := range legacyRows {
			normalized := model.TenantSiteConfig{}
			if row.Config != nil {
				if v, ok := row.Config["targetedInstanceCreation"]; ok {
					if enabled, ok := v.(bool); ok {
						normalized.TargetedInstanceCreation = &enabled
					}
				}
			}

			_, err = tx.NewUpdate().Model(&model.TenantSite{
				ID:     row.ID,
				Config: normalized,
			}).Column("config").WherePK().Exec(ctx)
			handleError(tx, err)
		}

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Moved tenant capabilities to Ready tenant_account.config and normalized tenant_site.config (tenant.config retained for rolling-deploy compatibility)")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] tenant_account.config")
		return nil
	})
}
