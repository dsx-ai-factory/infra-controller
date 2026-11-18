// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"encoding/json"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func testOperationRunReadAcrossAddedColumn(
	t *testing.T,
	read func(*PostgresStore, context.Context, uuid.UUID) (*operationrun.OperationRun, error),
) {
	t.Helper()
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping PostgreSQL operation-run test: no DB environment specified")
	}
	ctx := t.Context()
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(ctx, t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	// Retain the pgx connection and its prepared statements between transactions.
	session.DB.SetMaxOpenConns(1)
	session.DB.SetMaxIdleConns(1)
	store := NewPostgresStore(session)

	createdAt := time.Date(2026, 9, 1, 12, 0, 0, 0, time.UTC)
	startedAt := createdAt.Add(time.Minute)
	finishedAt := startedAt.Add(time.Hour)
	expected := dbmodel.OperationRun{
		ID:                uuid.New(),
		Name:              "projection run",
		Description:       "persist every operation-run field",
		Status:            operationrun.OperationRunStatusCompletedWithFailures,
		StatusReason:      operationrun.OperationRunStatusReasonNone,
		StatusMessage:     "one target failed",
		CurrentPhaseIndex: 2,
		TotalPhases:       3,
		Selector:          json.RawMessage(`{"rack_ids": ["11111111-1111-1111-1111-111111111111"]}`),
		Options:           json.RawMessage(`{"max_concurrent": 2}`),
		OperationTemplate: json.RawMessage(`{"operation_code": "power_on"}`),
		OperationType:     taskcommon.TaskTypePowerControl,
		OperationCode:     taskcommon.OpCodePowerControlPowerOn,
		CreatedAt:         createdAt,
		UpdatedAt:         finishedAt,
		StartedAt:         &startedAt,
		FinishedAt:        &finishedAt,
	}
	_, err = session.DB.NewInsert().Model(&expected).Exec(ctx)
	require.NoError(t, err)

	var backendPID int
	err = session.DB.QueryRowContext(ctx, "SELECT pg_backend_pid()").Scan(&backendPID)
	require.NoError(t, err)
	var preparedName string
	for _, phase := range []struct {
		name      string
		addColumn bool
	}{
		{name: "before column addition"},
		{name: "after column addition", addColumn: true},
	} {
		t.Run(phase.name, func(t *testing.T) {
			if phase.addColumn {
				_, err = session.DB.ExecContext(ctx, "ALTER TABLE operation_run ADD COLUMN test_added_column text")
				require.NoError(t, err)
			}
			var found *operationrun.OperationRun
			err = store.RunInTransaction(ctx, func(ctx context.Context) error {
				var readErr error
				found, readErr = read(store, ctx, expected.ID)
				return readErr
			})
			require.NoError(t, err)
			require.Equal(t, expected.ID, found.ID)
			require.Equal(t, expected.Name, found.Name)
			require.Equal(t, expected.Description, found.Description)
			require.Equal(t, expected.Status, found.Status)
			require.Equal(t, expected.StatusReason, found.StatusReason)
			require.Equal(t, expected.StatusMessage, found.StatusMessage)
			require.Equal(t, expected.CurrentPhaseIndex, found.CurrentPhaseIndex)
			require.Equal(t, expected.TotalPhases, found.TotalPhases)
			require.JSONEq(t, string(expected.Selector), string(found.Selector))
			require.JSONEq(t, string(expected.Options), string(found.Options))
			require.JSONEq(t, string(expected.OperationTemplate), string(found.OperationTemplate))
			require.Equal(t, expected.OperationType, found.OperationType)
			require.Equal(t, expected.OperationCode, found.OperationCode)
			require.True(t, expected.CreatedAt.Equal(found.CreatedAt))
			require.True(t, expected.UpdatedAt.Equal(found.UpdatedAt))
			require.NotNil(t, found.StartedAt)
			require.NotNil(t, found.FinishedAt)
			require.True(t, expected.StartedAt.Equal(*found.StartedAt))
			require.True(t, expected.FinishedAt.Equal(*found.FinishedAt))

			var currentPID int
			err = session.DB.QueryRowContext(ctx, "SELECT pg_backend_pid()").Scan(&currentPID)
			require.NoError(t, err)
			require.Equal(t, backendPID, currentPID)
			var currentPreparedName string
			err = session.DB.QueryRowContext(ctx, `SELECT name FROM pg_prepared_statements
				WHERE statement LIKE 'SELECT %FROM "operation_run" AS "orun" WHERE%'
				AND statement NOT LIKE '%pg_prepared_statements%'`).Scan(&currentPreparedName)
			require.NoError(t, err)
			if phase.addColumn {
				require.Equal(t, preparedName, currentPreparedName)
			} else {
				preparedName = currentPreparedName
			}
		})
	}
}
