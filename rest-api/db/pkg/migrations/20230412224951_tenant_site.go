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

type legacyTenantSiteInsert struct {
	bun.BaseModel `bun:"table:tenant_site,alias:ts"`

	ID                  uuid.UUID              `bun:"type:uuid,pk"`
	TenantID            uuid.UUID              `bun:"tenant_id,type:uuid,notnull"`
	TenantOrg           string                 `bun:"tenant_org,notnull"`
	SiteID              uuid.UUID              `bun:"site_id,type:uuid,notnull"`
	EnableSerialConsole bool                   `bun:"enable_serial_console,notnull"`
	Config              map[string]interface{} `bun:"config,type:jsonb,notnull,default:'{}'::jsonb"`
	CreatedBy           uuid.UUID              `bun:"type:uuid,notnull"`
}

func createAndPopulateTenantSiteUpMigrationfunc(ctx context.Context, db *bun.DB) error {
	// Start transactions
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Create TenantSite table
	_, err := tx.NewCreateTable().IfNotExists().Model((*model.TenantSite)(nil)).Exec(ctx)
	handleError(tx, err)

	// TenantSite map
	tenantSiteMap := make(map[string]bool)

	// Populate TenantSite table
	allocations := []model.Allocation{}
	err = tx.NewSelect().Model(&allocations).Relation(model.TenantRelationName).Scan(ctx)
	handleError(tx, err)

	// Create TenantSite entries
	count := 0
	for _, allocation := range allocations {
		mapKey := fmt.Sprintf("%s-%s", allocation.TenantID, allocation.SiteID)
		_, found := tenantSiteMap[mapKey]
		count++
		if !found {
			tenantSiteMap[mapKey] = true
			tenantSite := legacyTenantSiteInsert{
				ID:                  uuid.New(),
				TenantID:            allocation.TenantID,
				TenantOrg:           allocation.Tenant.Org,
				SiteID:              allocation.SiteID,
				EnableSerialConsole: false,
				Config:              map[string]interface{}{},
				CreatedBy:           allocation.CreatedBy,
			}
			_, err = tx.NewInsert().Model(&tenantSite).Exec(ctx)
			handleError(tx, err)
		}
	}

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Printf("Created %v TenantSite entries\n", count)
	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(createAndPopulateTenantSiteUpMigrationfunc, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
