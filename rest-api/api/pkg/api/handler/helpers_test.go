// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestPrepareProviderSiteHandler(t *testing.T) {
	tests := []struct {
		name        string
		code        int
		lookupError error
	}{
		{name: "Site access does not require Flow"},
		{name: "provider not found", code: http.StatusBadRequest},
		{name: "provider lookup failure", code: http.StatusInternalServerError, lookupError: errors.New("database connection failed")},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			db := testRackInitDB(t)
			defer db.Close()
			ctx := t.Context()
			org := "provider-site-helper-test"
			_, site, _ := testRackSetupTestData(t, db, org)
			user := testRackBuildUser(t, db, "provider-site-helper-user", org, []string{auth.ProviderAdminRole})
			site.Config = &cdbm.SiteConfig{Flow: false}
			_, err := db.DB.NewUpdate().Model(site).Column("config").WherePK().Exec(ctx)
			require.NoError(t, err)

			if tc.code != 0 {
				sqlDB, mock, mockErr := sqlmock.New()
				require.NoError(t, mockErr)
				mockDB := bun.NewDB(sqlDB, pgdialect.New())
				db = &cdb.Session{DB: mockDB}
				query := mock.ExpectQuery(`SELECT .* FROM "infrastructure_provider" AS "ip"`)
				if tc.lookupError != nil {
					query.WillReturnError(tc.lookupError)
				} else {
					query.WillReturnRows(sqlmock.NewRows([]string{"id"}))
				}
				mock.ExpectClose()
				t.Cleanup(func() {
					require.NoError(t, mockDB.Close())
					require.NoError(t, mock.ExpectationsWereMet())
				})
			}

			var logs bytes.Buffer
			resolved, apiErr := prepareProviderSiteHandler(ctx, db, user, org, site.ID.String(), zerolog.New(&logs))
			if tc.code != 0 {
				require.Nil(t, resolved)
				require.NotNil(t, apiErr)
				require.Equal(t, tc.code, apiErr.Code)
				require.Equal(t, "Failed to retrieve Infrastructure Provider for org", apiErr.Message)
				require.Nil(t, apiErr.Data)
				if tc.lookupError != nil {
					require.Contains(t, logs.String(), tc.lookupError.Error())
					require.NotContains(t, apiErr.Message, tc.lookupError.Error())
				}
				return
			}
			require.Nil(t, apiErr)
			require.NotNil(t, resolved)
			require.Equal(t, site.ID, resolved.ID)
			require.False(t, resolved.Config.Flow)
		})
	}
}

func TestPrepareFlowHandler(t *testing.T) {
	tests := []struct {
		name   string
		config *cdbm.SiteConfig
	}{
		{name: "missing config"},
		{name: "Flow disabled", config: &cdbm.SiteConfig{Flow: false}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			db := testRackInitDB(t)
			defer db.Close()
			ctx := context.Background()
			org := "flow-helper-test"
			_, site, _ := testRackSetupTestData(t, db, org)
			user := testRackBuildUser(t, db, "flow-helper-user", org, []string{auth.ProviderAdminRole})
			site.Config = tc.config
			_, err := db.DB.NewUpdate().Model(site).Column("config").WherePK().Exec(ctx)
			require.NoError(t, err)

			// A nil pool proves rejection happens before client retrieval.
			resolved, client, apiErr := prepareFlowHandler(ctx, db, nil, user, org, site.ID.String(), zerolog.Nop())
			require.Nil(t, resolved)
			require.Nil(t, client)
			require.NotNil(t, apiErr)
			require.Equal(t, http.StatusPreconditionFailed, apiErr.Code)
			require.Equal(t, "Site does not have NICo Flow enabled", apiErr.Message)
		})
	}
}
