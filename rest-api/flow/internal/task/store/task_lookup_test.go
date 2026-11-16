// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"database/sql"
	"errors"
	"os"
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestPostgresStore_GetTask(t *testing.T) {
	cases := []struct {
		name     string
		queryErr error
		wantCode codes.Code
	}{
		{name: "empty result is not found", wantCode: codes.NotFound},
		{name: "wrapped absence is not found", queryErr: errors.Join(errors.New("query failed"), sql.ErrNoRows), wantCode: codes.NotFound},
		{name: "database failure stays internal", queryErr: errors.New("database unavailable"), wantCode: codes.Internal},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			db := bun.NewDB(sqlDB, pgdialect.New())
			defer db.Close()
			store := NewPostgres(&cdb.Session{DB: db})
			query := mock.ExpectQuery(`SELECT .* FROM "task" .* WHERE .*id`)
			if tc.queryErr != nil {
				query.WillReturnError(tc.queryErr)
			} else {
				query.WillReturnRows(sqlmock.NewRows([]string{"id"}))
			}

			got, err := store.GetTask(t.Context(), uuid.New())

			require.Nil(t, got)
			require.Equal(t, tc.wantCode, status.Code(err))
			if tc.wantCode == codes.NotFound {
				require.Equal(t, "task not found", status.Convert(err).Message())
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}

func TestPostgresStore_GetTasks(t *testing.T) {
	first, second := uuid.New(), uuid.New()
	cases := []struct {
		name          string
		missingSecond bool
		queryErr      error
		wantCode      codes.Code
	}{
		{name: "succeeded and failed tasks retain request order", wantCode: codes.OK},
		{name: "missing task rejects the whole lookup", missingSecond: true, wantCode: codes.NotFound},
		{name: "database failure rejects the whole lookup", queryErr: errors.New("database unavailable"), wantCode: codes.Internal},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			db := bun.NewDB(sqlDB, pgdialect.New())
			defer db.Close()
			store := NewPostgres(&cdb.Session{DB: db})
			mock.ExpectQuery(`SELECT .* FROM "task" .* WHERE .*` + first.String()).
				WillReturnRows(sqlmock.NewRows([]string{"id", "status"}).AddRow(first, taskcommon.TaskStatusCompleted))
			query := mock.ExpectQuery(`SELECT .* FROM "task" .* WHERE .*` + second.String())
			switch {
			case tc.queryErr != nil:
				query.WillReturnError(tc.queryErr)
			case tc.missingSecond:
				query.WillReturnRows(sqlmock.NewRows([]string{"id", "status"}))
			default:
				query.WillReturnRows(sqlmock.NewRows([]string{"id", "status"}).AddRow(second, taskcommon.TaskStatusFailed))
			}

			got, err := store.GetTasks(t.Context(), []uuid.UUID{first, second})

			require.Equal(t, tc.wantCode, status.Code(err))
			if tc.wantCode == codes.OK {
				require.Len(t, got, 2)
				require.Equal(t, first, got[0].ID)
				require.Equal(t, taskcommon.TaskStatusCompleted, got[0].Status)
				require.Equal(t, second, got[1].ID)
				require.Equal(t, taskcommon.TaskStatusFailed, got[1].Status)
			} else {
				require.Nil(t, got)
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}

	t.Run("postgres absence", func(t *testing.T) {
		if os.Getenv("DB_PORT") == "" {
			t.Skip("Skipping PostgreSQL task store test: no DB environment specified")
		}
		config, err := cdb.ConfigFromEnv()
		require.NoError(t, err)
		session, err := utils.UnitTestDB(context.Background(), t, config)
		require.NoError(t, err)
		t.Cleanup(session.Close)

		got, err := NewPostgres(session).GetTasks(t.Context(), []uuid.UUID{uuid.New()})

		require.Nil(t, got)
		require.Equal(t, codes.NotFound, status.Code(err))
		require.Equal(t, "task not found", status.Convert(err).Message())
	})
}
