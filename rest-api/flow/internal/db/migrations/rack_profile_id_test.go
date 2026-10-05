// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations_test

import (
	"context"
	_ "embed"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/db/model"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
)

//go:embed 20260928234309_rack_profile_id.up.sql
var rackProfileIDUp string

//go:embed 20260928234309_rack_profile_id.down.sql
var rackProfileIDDown string

func TestRackProfileIDMigration(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)
	_, err := session.DB.ExecContext(ctx, rackProfileIDDown)
	require.NoError(t, err)
	id := uuid.New()
	_, err = session.DB.ExecContext(ctx, "INSERT INTO rack (id, name) VALUES (?, 'legacy')", id)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, rackProfileIDUp)
	require.NoError(t, err)
	var stored model.Rack
	require.NoError(t, session.DB.NewSelect().Model(&stored).Where("id = ?", id).Scan(ctx))
	require.Nil(t, stored.RackProfileID)
	require.Equal(t, "legacy", stored.Name)
	_, err = session.DB.ExecContext(ctx, "INSERT INTO rack (name) VALUES ('old-writer')")
	require.NoError(t, err, "predecessor writers may omit the new column")
	_, err = session.DB.ExecContext(ctx, rackProfileIDDown)
	require.NoError(t, err)
	var count int
	require.NoError(t, session.DB.NewRaw("SELECT count(*) FROM rack").Scan(ctx, &count))
	require.Equal(t, 2, count)
}
