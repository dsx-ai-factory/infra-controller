// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/migrate"
)

func TestExpectedRackImmediateUniqueUpMigration(t *testing.T) {
	ctx := context.Background()
	session := util.GetTestDBSession(t, true)
	defer session.Close()
	model.TestSetupSchema(t, session)
	user := model.TestBuildUser(t, session, uuid.NewString(), "rack-unique", []string{"FORGE_PROVIDER_ADMIN"})
	provider := model.TestBuildInfrastructureProvider(t, session, "provider", "rack-unique", user)
	site := model.TestBuildSite(t, session, provider, "site", user)
	otherSite := model.TestBuildSite(t, session, provider, "other-site", user)
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260429100000" {
			require.NoError(t, migration.Up(ctx, session.DB))
		}
	}
	dao := model.NewExpectedRackDAO(session)
	seed := model.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(), SiteID: site.ID, RackID: "existing-rack",
		RackProfileID: "existing-profile", Name: "Existing rack", CreatedBy: user.ID,
		Labels: map[string]string{"location.room": "lab"},
	}
	before, err := dao.Create(ctx, nil, seed)
	require.NoError(t, err)
	target := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20261006123500" {
			target.Add(migration)
		}
	}
	require.Len(t, target.Sorted(), 1)
	migrator := migrate.NewMigrator(session.DB, target, migrate.WithMarkAppliedOnSuccess(true))
	require.NoError(t, migrator.Init(ctx))
	_, err = migrator.Migrate(ctx)
	require.NoError(t, err)

	for _, tc := range []struct {
		name  string
		check func(*testing.T)
	}{
		{
			name: "preserves rows and site scoped uniqueness",
			check: func(t *testing.T) {
				stored, err := dao.Get(ctx, nil, before.ID, nil, false)
				require.NoError(t, err)
				require.Equal(t, before, stored)
				input := seed
				input.ExpectedRackID = uuid.New()
				_, err = dao.Create(ctx, nil, input)
				var pgErr *pgconn.PgError
				require.ErrorAs(t, err, &pgErr)
				require.Equal(t, "23505", pgErr.Code)
				input.SiteID = otherSite.ID
				_, err = dao.Create(ctx, nil, input)
				require.NoError(t, err)
			},
		},
		{
			name: "API profile readback commits while inventory inserts the same rack",
			check: func(t *testing.T) {
				raceCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
				defer cancel()
				tx, err := cdb.BeginTx(raceCtx, session, nil)
				require.NoError(t, err)
				defer func() { _ = tx.Rollback() }()
				var apiPID int
				err = tx.GetBunTx().QueryRowContext(raceCtx, "SELECT pg_backend_pid()").Scan(&apiPID)
				require.NoError(t, err)
				input := seed
				input.ExpectedRackID = uuid.New()
				input.RackID = "concurrent-rack"
				input.RackProfileID = ""
				created, err := dao.Create(raceCtx, tx, input)
				require.NoError(t, err)
				inventoryResult := make(chan error, 1)
				go func() {
					input.ExpectedRackID = uuid.New()
					input.RackProfileID = "derived-profile"
					_, err := dao.Create(raceCtx, nil, input)
					inventoryResult <- err
				}()
				require.Eventually(t, func() bool {
					var blocked bool
					err := session.DB.QueryRowContext(raceCtx, `SELECT EXISTS (
						SELECT 1 FROM pg_stat_activity WHERE ? = ANY(pg_blocking_pids(pid)))`, apiPID).Scan(&blocked)
					return err == nil && blocked
				}, 3*time.Second, 10*time.Millisecond, "inventory must be waiting on the uncommitted API row")
				_, err = dao.Update(raceCtx, tx, model.ExpectedRackUpdateInput{
					ExpectedRackID: created.ID, RackProfileID: cutil.GetPtr("derived-profile"),
				})
				require.NoError(t, err)
				err = tx.Commit()
				require.NoError(t, err)
				select {
				case err = <-inventoryResult:
				case <-raceCtx.Done():
					t.Fatal("inventory insert did not finish after API commit")
				}
				var pgErr *pgconn.PgError
				require.ErrorAs(t, err, &pgErr)
				require.Equal(t, "23505", pgErr.Code)
				stored, err := dao.Get(ctx, nil, created.ID, nil, false)
				require.NoError(t, err)
				require.Equal(t, "derived-profile", stored.RackProfileID)
				require.Equal(t, user.ID, stored.CreatedBy)
			},
		},
		{
			name: "replacement reuses rack identity within the transaction",
			check: func(t *testing.T) {
				input := seed
				input.ExpectedRackID = uuid.New()
				input.RackProfileID = "replacement-profile"
				err := cdb.WithTx(ctx, session, func(tx *cdb.Tx) error {
					_, err := dao.ReplaceAll(ctx, tx, model.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{site.ID}}, []model.ExpectedRackCreateInput{input})
					return err
				})
				require.NoError(t, err)
				stored, err := dao.Get(ctx, nil, input.ExpectedRackID, nil, false)
				require.NoError(t, err)
				require.Equal(t, "replacement-profile", stored.RackProfileID)
			},
		},
	} {
		t.Run(tc.name, tc.check)
	}
}
