// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/uptrace/bun"
)

// 20260623150000_ipxe_os_and_templates
//
// Adds the schema needed for the Templated iPXE Operating System variant and its
// synchronization with nico-core:
//   - ipxe_template: global iPXE script templates, keyed by the stable UUID assigned
//     by nico-core (the same UUID is used on both sides).
//   - ipxe_template_site_association (ITSA): tracks which sites currently report each
//     template.
//   - operating_system: iPXE template definition columns plus the ipxe_os_scope column
//     that controls synchronization direction.
//   - operating_system_site_association: controller_state column mirroring the per-site
//     tenant state reported by nico-core.
//
// This migration is additive. The legacy controller_operating_system_id column is
// intentionally left in place; consolidating on a shared OS UUID is handled separately
// once all readers have been updated.
func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// iPXE Template table (global). The unique name constraint and column types
		// come from the bun model definition.
		_, err := tx.NewCreateTable().Model((*model.IpxeTemplate)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS ipxe_template_name_idx ON ipxe_template(name)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS ipxe_template_visibility_idx ON ipxe_template(visibility)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS ipxe_template_created_idx ON ipxe_template(created)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS ipxe_template_updated_idx ON ipxe_template(updated)")
		handleError(tx, err)

		// iPXE Template <-> Site association. Foreign keys are declared on the model.
		_, err = tx.NewCreateTable().Model((*model.IpxeTemplateSiteAssociation)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)

		_, err = tx.Exec(`
			ALTER TABLE ipxe_template_site_association
			DROP CONSTRAINT IF EXISTS ipxe_template_site_association_template_id_site_id_key
		`)
		handleError(tx, err)
		_, err = tx.Exec(`
			ALTER TABLE ipxe_template_site_association
			ADD CONSTRAINT ipxe_template_site_association_template_id_site_id_key
			UNIQUE (ipxe_template_id, site_id)
		`)
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS itsa_ipxe_template_id_idx ON ipxe_template_site_association(ipxe_template_id)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS itsa_site_id_idx ON ipxe_template_site_association(site_id)")
		handleError(tx, err)

		// Operating System: iPXE template definition columns.
		_, err = tx.Exec("ALTER TABLE operating_system ADD COLUMN IF NOT EXISTS ipxe_template_id TEXT NULL")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system ADD COLUMN IF NOT EXISTS ipxe_template_parameters JSONB NULL")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system ADD COLUMN IF NOT EXISTS ipxe_template_artifacts JSONB NULL")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system ADD COLUMN IF NOT EXISTS ipxe_template_definition_hash TEXT NULL")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system ADD COLUMN IF NOT EXISTS ipxe_os_scope TEXT NULL")
		handleError(tx, err)

		// Operating System Site Association: per-site controller state.
		_, err = tx.Exec("ALTER TABLE operating_system_site_association ADD COLUMN IF NOT EXISTS controller_state TEXT NULL")
		handleError(tx, err)

		// Backfill ipxe_os_scope for existing iPXE-type OS records. Image rows keep a
		// NULL scope since scope does not apply to them.
		//   tenant-owned iPXE  -> Global (carbide-rest is the source of truth)
		//   provider-owned iPXE -> Local  (bidirectional with nico-core)
		_, err = tx.Exec(`
			UPDATE operating_system
			SET ipxe_os_scope = 'Global'
			WHERE ipxe_os_scope IS NULL
			  AND type = 'iPXE'
			  AND tenant_id IS NOT NULL
			  AND deleted IS NULL
		`)
		handleError(tx, err)
		_, err = tx.Exec(`
			UPDATE operating_system
			SET ipxe_os_scope = 'Local'
			WHERE ipxe_os_scope IS NULL
			  AND type = 'iPXE'
			  AND tenant_id IS NULL
			  AND deleted IS NULL
		`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added iPXE template tables and Operating System scope/template columns. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		_, err := tx.Exec("ALTER TABLE operating_system_site_association DROP COLUMN IF EXISTS controller_state")
		handleError(tx, err)

		_, err = tx.Exec("ALTER TABLE operating_system DROP COLUMN IF EXISTS ipxe_os_scope")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system DROP COLUMN IF EXISTS ipxe_template_definition_hash")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system DROP COLUMN IF EXISTS ipxe_template_artifacts")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system DROP COLUMN IF EXISTS ipxe_template_parameters")
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE operating_system DROP COLUMN IF EXISTS ipxe_template_id")
		handleError(tx, err)

		_, err = tx.Exec("DROP TABLE IF EXISTS ipxe_template_site_association")
		handleError(tx, err)
		_, err = tx.Exec("DROP TABLE IF EXISTS ipxe_template")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [down migration] Dropped iPXE template tables and Operating System scope/template columns. ")
		return nil
	})
}
