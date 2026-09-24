// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func newListOrderTestDB(t *testing.T) (*bun.DB, sqlmock.Sqlmock) {
	t.Helper()
	sqlDB, mock, err := sqlmock.New()
	require.NoError(t, err)
	db := bun.NewDB(sqlDB, pgdialect.New())
	t.Cleanup(func() {
		_ = db.Close()
	})
	return db, mock
}

func TestInventoryLists_StableOrderBeforePagination(t *testing.T) {
	page := &dbquery.Pagination{Offset: 0, Limit: 20, Total: 1}
	info := dbquery.StringQueryInfo{}

	t.Run("components use default name and ID order", func(t *testing.T) {
		db, mock := newListOrderTestDB(t)
		mock.ExpectQuery(`ORDER BY "c"\."name" ASC, "c"\."id" ASC LIMIT 20`).
			WillReturnRows(sqlmock.NewRows([]string{"id"}))

		_, _, err := GetListOfComponents(t.Context(), db, info, nil, nil, nil, page, nil)

		require.NoError(t, err)
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("components append ID to requested order", func(t *testing.T) {
		db, mock := newListOrderTestDB(t)
		mock.ExpectQuery(`ORDER BY "c"\."manufacturer" DESC, "c"\."id" ASC LIMIT 20`).
			WillReturnRows(sqlmock.NewRows([]string{"id"}))

		_, _, err := GetListOfComponents(t.Context(), db, info, nil, nil, nil, page, &dbquery.OrderBy{
			Column: "manufacturer", Direction: dbquery.OrderDescending,
		})

		require.NoError(t, err)
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("racks use default name and ID order", func(t *testing.T) {
		db, mock := newListOrderTestDB(t)
		mock.ExpectQuery(`ORDER BY "name" ASC, "id" ASC LIMIT 20`).
			WillReturnRows(sqlmock.NewRows([]string{"id"}))

		_, _, err := GetListOfRacks(t.Context(), db, info, nil, nil, page, nil, false)

		require.NoError(t, err)
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("racks append ID to requested order", func(t *testing.T) {
		db, mock := newListOrderTestDB(t)
		mock.ExpectQuery(`ORDER BY "manufacturer" DESC, "id" ASC LIMIT 20`).
			WillReturnRows(sqlmock.NewRows([]string{"id"}))

		_, _, err := GetListOfRacks(t.Context(), db, info, nil, nil, page, &dbquery.OrderBy{
			Column: "manufacturer", Direction: dbquery.OrderDescending,
		}, false)

		require.NoError(t, err)
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("NVLink domains use default name and ID order", func(t *testing.T) {
		db, mock := newListOrderTestDB(t)
		mock.ExpectQuery(`ORDER BY "name" ASC, "id" ASC LIMIT 20`).
			WillReturnRows(sqlmock.NewRows([]string{"id"}))

		_, _, err := GetListOfNVLDomains(t.Context(), db, info, page)

		require.NoError(t, err)
		require.NoError(t, mock.ExpectationsWereMet())
	})
}

func TestListOperationRules_StableOrderBeforePagination(t *testing.T) {
	db, mock := newListOrderTestDB(t)
	mock.ExpectQuery(`ORDER BY "created_at" DESC, "id" DESC LIMIT 20`).
		WillReturnRows(sqlmock.NewRows([]string{"id"}))

	_, _, err := ListOperationRules(
		t.Context(),
		db,
		&taskcommon.OperationRuleListOptions{},
		&dbquery.Pagination{Offset: 0, Limit: 20, Total: 1},
	)

	require.NoError(t, err)
	require.NoError(t, mock.ExpectationsWereMet())
}
