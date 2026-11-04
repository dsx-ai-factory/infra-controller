// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package taskschedule

import (
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
)

func TestPostgresStore_Disable(t *testing.T) {
	sqlDB, mock, err := sqlmock.New()
	require.NoError(t, err)
	defer sqlDB.Close()

	db := bun.NewDB(sqlDB, pgdialect.New())
	defer db.Close()
	store := NewPostgresStore(&cdb.Session{DB: db})
	id := uuid.MustParse("aaaaaaaa-0000-0000-0000-000000000001")

	mock.ExpectExec(
		`UPDATE task_schedule SET enabled = FALSE, next_run_at = NULL WHERE \(id =`,
	).WillReturnResult(sqlmock.NewResult(0, 1))

	require.NoError(t, store.Disable(t.Context(), id))
	require.NoError(t, mock.ExpectationsWereMet())
}

func TestPostgresStore_List_StableOrderBeforePagination(t *testing.T) {
	sqlDB, mock, err := sqlmock.New()
	require.NoError(t, err)
	defer sqlDB.Close()

	db := bun.NewDB(sqlDB, pgdialect.New())
	defer db.Close()
	store := NewPostgresStore(&cdb.Session{DB: db})

	mock.ExpectQuery(`SELECT count\(\*\) FROM "task_schedule" AS "ts"`).
		WillReturnRows(sqlmock.NewRows([]string{"count"}).AddRow(1))
	mock.ExpectQuery(`ORDER BY ts\.created_at ASC, ts\.id ASC LIMIT 20`).
		WillReturnRows(sqlmock.NewRows([]string{"id"}))

	_, total, err := store.List(t.Context(), ListOptions{
		Pagination: &dbquery.Pagination{Offset: 0, Limit: 20},
	})

	require.NoError(t, err)
	require.Equal(t, int32(1), total)
	require.NoError(t, mock.ExpectationsWereMet())
}
