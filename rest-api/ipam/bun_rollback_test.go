/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"bytes"
	"context"
	dbsql "database/sql"
	"errors"
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
)

func TestBundb_CreatePrefix(t *testing.T) {
	for _, tc := range []struct {
		name            string
		rollbackErr     error
		userTransaction bool
	}{
		{name: "insert failure rolls back owned transaction"},
		{name: "rollback failure is logged without replacing insert error", rollbackErr: errors.New("rollback connection failure")},
		{name: "insert failure leaves caller transaction open", userTransaction: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			bdb := bun.NewDB(sqlDB, pgdialect.New())
			t.Cleanup(func() { mock.ExpectClose(); require.NoError(t, bdb.Close()) })
			storage := NewBunStorage(bdb, nil)
			if tc.userTransaction {
				mock.ExpectBegin()
				tx, err := bdb.BeginTx(context.Background(), nil)
				require.NoError(t, err)
				storage.userTx = &tx
			}
			mock.ExpectQuery("SELECT").WillReturnError(dbsql.ErrNoRows)
			if !tc.userTransaction {
				mock.ExpectBegin()
			}
			insertErr := errors.New("insert prefix failed")
			mock.ExpectExec("INSERT INTO").WillReturnError(insertErr)
			if !tc.userTransaction {
				mock.ExpectRollback().WillReturnError(tc.rollbackErr)
			}
			var logs bytes.Buffer
			original := log.Logger
			log.Logger = zerolog.New(&logs)
			t.Cleanup(func() { log.Logger = original })
			_, err = storage.CreatePrefix(context.Background(), Prefix{Cidr: "10.0.0.0/24"}, "")
			require.ErrorIs(t, err, insertErr)
			if tc.rollbackErr != nil {
				assert.Contains(t, logs.String(), tc.rollbackErr.Error())
				assert.Contains(t, logs.String(), "failed to rollback transaction")
			} else {
				assert.Empty(t, logs.String())
			}
			if tc.userTransaction {
				mock.ExpectExec("SELECT 1").WillReturnResult(sqlmock.NewResult(0, 1))
				_, err := storage.userTx.Exec("SELECT 1")
				require.NoError(t, err)
				mock.ExpectRollback()
				require.NoError(t, storage.userTx.Rollback())
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}
