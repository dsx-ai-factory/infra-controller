// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"bytes"
	"context"
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

func TestRollbackTx(t *testing.T) {
	rollbackErr := errors.New("rollback connection failure")
	for _, tc := range []struct {
		name          string
		rollbackErr   error
		committed     bool
		nilFlag       bool
		alreadyClosed bool
		wantLog       bool
	}{
		{name: "successful rollback"},
		{name: "rollback failure is logged", rollbackErr: rollbackErr, wantLog: true},
		{name: "committed transaction is skipped", committed: true},
		{name: "nil flag is skipped", nilFlag: true},
		{name: "already closed transaction is quiet", alreadyClosed: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			bdb := bun.NewDB(sqlDB, pgdialect.New())
			t.Cleanup(func() {
				mock.ExpectClose()
				require.NoError(t, bdb.Close())
			})
			mock.ExpectBegin()
			btx, err := bdb.BeginTx(context.Background(), nil)
			require.NoError(t, err)
			tx := &Tx{tx: btx}
			if tc.alreadyClosed || (!tc.committed && !tc.nilFlag) {
				mock.ExpectRollback().WillReturnError(tc.rollbackErr)
			}
			if tc.alreadyClosed {
				require.NoError(t, tx.Rollback())
			}
			var logs bytes.Buffer
			original := log.Logger
			log.Logger = zerolog.New(&logs)
			t.Cleanup(func() { log.Logger = original })
			committed := &tc.committed
			if tc.nilFlag {
				committed = nil
			}
			RollbackTx(context.Background(), tx, committed)
			if tc.wantLog {
				assert.Contains(t, logs.String(), rollbackErr.Error())
				assert.Contains(t, logs.String(), "failed to rollback transaction")
			} else {
				assert.Empty(t, logs.String())
			}
			require.NoError(t, mock.ExpectationsWereMet())
			if tc.committed || tc.nilFlag {
				mock.ExpectRollback()
				require.NoError(t, tx.Rollback())
			}
		})
	}
}

func TestBeginTx(t *testing.T) {
	for _, tc := range []struct {
		name        string
		rollbackErr error
		wantLog     bool
	}{
		{name: "setup error is preserved"},
		{name: "rollback failure is logged without replacing setup error", rollbackErr: errors.New("rollback connection failure"), wantLog: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			bdb := bun.NewDB(sqlDB, pgdialect.New())
			t.Cleanup(func() {
				mock.ExpectClose()
				require.NoError(t, bdb.Close())
			})
			setupErr := errors.New("lock timeout setup failed")
			mock.ExpectBegin()
			mock.ExpectExec("SET LOCAL lock_timeout").WillReturnError(setupErr)
			mock.ExpectRollback().WillReturnError(tc.rollbackErr)
			var logs bytes.Buffer
			original := log.Logger
			log.Logger = zerolog.New(&logs)
			t.Cleanup(func() { log.Logger = original })
			tx, err := BeginTx(context.Background(), &Session{DB: bdb}, nil)
			require.ErrorIs(t, err, setupErr)
			assert.Nil(t, tx)
			if tc.wantLog {
				assert.Contains(t, logs.String(), tc.rollbackErr.Error())
			} else {
				assert.Empty(t, logs.String())
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}
