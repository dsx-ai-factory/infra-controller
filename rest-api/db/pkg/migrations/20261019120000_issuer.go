// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	// Each row is one external JWT issuer, written from the ConfigMap at pod
	// start (source config) or through the Auth Issuer API (source api). The
	// live auth registry is built from this table. gen_random_uuid() is built
	// in from PostgreSQL 13, the minimum this deployment supports.
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `
			CREATE TABLE issuer (
				id                   UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
				origin               TEXT        NOT NULL DEFAULT 'custom',
				source               TEXT        NOT NULL DEFAULT 'api',
				issuer_url           TEXT        NOT NULL,
				jwks_url             TEXT        NOT NULL,
				jwks_timeout         TEXT        NOT NULL DEFAULT '5s',
				service_account      BOOLEAN     NOT NULL DEFAULT false,
				audiences            TEXT[]      NOT NULL DEFAULT '{}',
				scopes               TEXT[]      NOT NULL DEFAULT '{}',
				claim_mappings       JSONB       NOT NULL DEFAULT '[]',
				-- Cached key set fetched from jwks_url; NULL means never fetched.
				-- Public key material only.
				jwks_keys            JSONB,
				jwks_fetched_at      TIMESTAMPTZ,
				jwks_last_attempt_at TIMESTAMPTZ,
				jwks_last_error      TEXT,
				status               TEXT        NOT NULL DEFAULT 'Pending',
				status_message       TEXT,
				-- Helm release revision of the ConfigMap a config row was last synced
				-- from; a replica on an older revision does not sync over it.
				config_revision      INTEGER,
				created_at           TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
				updated_at           TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
				created_by           UUID,
				updated_by           UUID,
				deleted              TIMESTAMPTZ
			);
			-- issuer_url is the token iss and the registry key.
			CREATE UNIQUE INDEX issuer_issuer_url_idx ON issuer(issuer_url) WHERE deleted IS NULL`)
		return err
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, "DROP TABLE issuer")
		return err
	})
}
