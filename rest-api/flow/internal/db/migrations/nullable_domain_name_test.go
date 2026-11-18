// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations_test

import (
	_ "embed"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
)

//go:embed 20260930235900_nullable_domain_name.up.sql
var nullableDomainNameUp string

//go:embed 20260930235900_nullable_domain_name.down.sql
var nullableDomainNameDown string

func TestNullableDomainNameMigration(t *testing.T) {
	ctx := t.Context()
	session := storetest.NewPostgresTestSession(t)
	_, err := session.DB.ExecContext(ctx, nullableDomainNameDown)
	require.NoError(t, err)
	legacyID := uuid.New()
	_, err = session.DB.ExecContext(ctx, "INSERT INTO nvldomain (id, name) VALUES (?, 'legacy-name')", legacyID)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, nullableDomainNameUp)
	require.NoError(t, err)
	legacy, err := (&model.NVLDomain{ID: legacyID}).Get(ctx, session.DB)
	require.NoError(t, err)
	require.Equal(t, "legacy-name", legacy.Name)
	_, err = session.DB.ExecContext(ctx, "INSERT INTO nvldomain (name) VALUES ('old-writer')")
	require.NoError(t, err, "predecessor writers can still insert named domains")
	_, err = session.DB.ExecContext(ctx, "INSERT INTO nvldomain (name) VALUES ('legacy-name')")
	require.Error(t, err, "nonempty domain names remain unique")

	ids := []uuid.UUID{
		uuid.MustParse("10000000-0000-0000-0000-000000000001"),
		uuid.MustParse("10000000-0000-0000-0000-000000000002"),
	}
	for i, group := range []string{"group-a", "group-b"} {
		domain := model.NVLDomain{ID: ids[i], ExternalID: &group}
		require.NoError(t, domain.Create(ctx, session.DB))
		stored, err := (&model.NVLDomain{ExternalID: &group}).Get(ctx, session.DB)
		require.NoError(t, err)
		require.Empty(t, stored.Name)
	}
	var unnamed int
	require.NoError(t, session.DB.NewRaw("SELECT count(*) FROM nvldomain WHERE name IS NULL").Scan(ctx, &unnamed))
	require.Equal(t, 2, unnamed)
	for offset := range 3 {
		rows, total, err := model.GetListOfNVLDomains(ctx, session.DB, dbquery.StringQueryInfo{}, &dbquery.Pagination{Offset: offset, Limit: 1}, nvldomain.ListOptions{ExternalOnly: true})
		require.NoError(t, err)
		require.EqualValues(t, 2, total)
		if offset == len(ids) {
			require.Empty(t, rows)
			continue
		}
		require.Len(t, rows, 1)
		require.Equal(t, ids[offset], rows[0].ID, "unnamed domains retain deterministic pagination")
	}
	_, err = session.DB.ExecContext(ctx, nullableDomainNameDown)
	require.Error(t, err, "rollback must refuse unnamed domains without rewriting their names")
	require.NoError(t, session.DB.NewRaw("SELECT count(*) FROM nvldomain WHERE name IS NULL").Scan(ctx, &unnamed))
	require.Equal(t, 2, unnamed)
	_, err = session.DB.ExecContext(ctx, "UPDATE nvldomain SET name = external_id WHERE name IS NULL")
	require.NoError(t, err, "simulate explicit operator naming before rollback")
	_, err = session.DB.ExecContext(ctx, nullableDomainNameDown)
	require.NoError(t, err)
}
