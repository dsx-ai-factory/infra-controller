// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"database/sql"
	"errors"
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type fakeSQLResult struct {
	rowsAffected int64
	err          error
}

func (r fakeSQLResult) LastInsertId() (int64, error) {
	return 0, nil
}

func (r fakeSQLResult) RowsAffected() (int64, error) {
	return r.rowsAffected, r.err
}

func newOfflineBun() *bun.DB {
	return bun.NewDB(nil, pgdialect.New())
}

func newMockPostgresStore(t *testing.T) (*PostgresStore, sqlmock.Sqlmock) {
	t.Helper()

	sqlDB, mock, err := sqlmock.New()
	require.NoError(t, err)
	t.Cleanup(func() { _ = sqlDB.Close() })

	return &PostgresStore{
		pg: &cdb.Session{
			DB: bun.NewDB(sqlDB, pgdialect.New()),
		},
	}, mock
}

func TestPostgresStore_Get(t *testing.T) {
	t.Run("preserves no rows for missing run", func(t *testing.T) {
		store, mock := newMockPostgresStore(t)
		id := uuid.MustParse("11111111-1111-1111-1111-111111111111")
		mock.ExpectQuery("SELECT").
			WillReturnError(sql.ErrNoRows)

		run, err := store.Get(context.Background(), id)

		require.Nil(t, run)
		require.ErrorIs(t, err, sql.ErrNoRows)
		require.ErrorContains(t, err, "operation run "+id.String()+" not found")
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("returns persisted phase count", func(t *testing.T) {
		store, mock := newMockPostgresStore(t)
		id := uuid.MustParse("11111111-1111-1111-1111-111111111111")
		mock.ExpectQuery(`SELECT orun\.\* FROM "operation_run" AS "orun"`).
			WillReturnRows(
				sqlmock.NewRows([]string{"id", "total_phases"}).
					AddRow(id, 2),
			)

		run, err := store.Get(context.Background(), id)

		require.NoError(t, err)
		require.Equal(t, int32(2), run.TotalPhases)
		require.NoError(t, mock.ExpectationsWereMet())
	})
}

func TestPostgresStore_LockOperationRunReturnsPersistedPhaseCount(t *testing.T) {
	store, mock := newMockPostgresStore(t)
	id := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	mock.ExpectQuery(`(?s)SELECT orun\.\* FROM "operation_run" AS "orun".*FOR UPDATE`).
		WillReturnRows(
			sqlmock.NewRows([]string{"id", "total_phases"}).
				AddRow(id, 2),
		)

	run, err := store.LockOperationRun(context.Background(), id)

	require.NoError(t, err)
	require.Equal(t, int32(2), run.TotalPhases)
	require.NoError(t, mock.ExpectationsWereMet())
}

func TestPostgresStore_List(t *testing.T) {
	store, mock := newMockPostgresStore(t)
	id := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	mock.ExpectQuery(`SELECT count\(\*\) FROM "operation_run" AS "orun"`).
		WillReturnRows(sqlmock.NewRows([]string{"count"}).AddRow(1))
	mock.ExpectQuery(`(?s)SELECT.*"total_phases".*FROM "operation_run" AS "orun".*ORDER BY orun\.created_at DESC, orun\.id DESC`).
		WillReturnRows(
			sqlmock.NewRows([]string{"id", "total_phases"}).
				AddRow(id, 2),
		)

	runs, total, err := store.List(context.Background(), operationrun.ListOptions{})

	require.NoError(t, err)
	require.Equal(t, int32(1), total)
	require.Len(t, runs, 1)
	require.Equal(t, int32(2), runs[0].TotalPhases)
	require.NoError(t, mock.ExpectationsWereMet())
}

func TestPostgresStore_GetTargetPhaseAggregate(t *testing.T) {
	store, mock := newMockPostgresStore(t)
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	mock.ExpectQuery(`(?s)SELECT.*completed_targets.*FROM operation_run_target AS ort`).
		WillReturnRows(
			sqlmock.NewRows([]string{
				"completed_targets",
				"completed_completed",
				"completed_failed",
				"completed_terminated",
				"completed_skipped",
			}).AddRow(1, 1, 0, 0, 0),
		)

	aggregate, err := store.GetTargetPhaseAggregate(context.Background(), runID, 1)

	require.NoError(t, err)
	require.Equal(t, 1, aggregate.CompletedPhaseStats.SelectedTargets)
	require.NoError(t, mock.ExpectationsWereMet())
}

func TestCreateTargetsValidatesComponentsByType(t *testing.T) {
	runID := uuid.New()
	componentID := uuid.New()

	tests := []struct {
		name             string
		componentsByType operation.ComponentsByType
		wantErr          string
	}{
		{
			name:             "empty map",
			componentsByType: operation.ComponentsByType{},
			wantErr:          "Non-empty ComponentsByType is required",
		},
		{
			name: "unknown component type",
			componentsByType: operation.ComponentsByType{
				devicetypes.ComponentTypeUnknown: {componentID},
			},
			wantErr: "ComponentsByType contains unknown component type",
		},
		{
			name: "empty component UUID",
			componentsByType: operation.ComponentsByType{
				devicetypes.ComponentTypeCompute: {uuid.Nil},
			},
			wantErr: "contains empty component UUID",
		},
		{
			name: "duplicate component UUID",
			componentsByType: operation.ComponentsByType{
				devicetypes.ComponentTypeCompute: {componentID, componentID},
			},
			wantErr: "duplicates component",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &PostgresStore{}

			err := store.CreateTargets(
				context.Background(),
				runID,
				[]*operationrun.OperationRunTarget{
					{
						RackID:           uuid.New(),
						ComponentsByType: tt.componentsByType,
					},
				},
			)

			require.ErrorContains(t, err, "operation run target 0 components_by_type")
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestApplyTargetPhaseScopeGeneratedSQL(t *testing.T) {
	tests := []struct {
		name    string
		scope   operationrun.TargetPhaseScope
		want    []string
		wantNot []string
	}{
		{
			name:  "current phase uses parent phase index",
			scope: operationrun.TargetPhaseScopeCurrentPhase,
			want: []string{
				"ort.phase_index = (",
				"SELECT current_phase_run.current_phase_index",
				"FROM operation_run AS current_phase_run",
			},
			wantNot: []string{
				"MAX(phase_index)",
				"COALESCE(",
				"current_phase.status NOT IN",
			},
		},
		{
			name:  "completed phases compare against parent phase index",
			scope: operationrun.TargetPhaseScopeCompletedPhases,
			want: []string{
				"ort.phase_index < COALESCE(",
				"SELECT current_phase_run.current_phase_index",
				"ort.phase_index + 1",
			},
			wantNot: []string{
				"MAX(phase_index)",
				"current_phase.status NOT IN",
			},
		},
		{
			name:  "current and completed phases stops at current phase",
			scope: operationrun.TargetPhaseScopeCurrentAndCompletedPhases,
			want: []string{
				"ort.phase_index <= COALESCE(",
				"SELECT current_phase_run.current_phase_index",
				"ort.phase_index)",
			},
			wantNot: []string{
				"MAX(phase_index)",
				"current_phase.status NOT IN",
			},
		},
		{
			name:  "all materialized targets does not apply phase scope",
			scope: operationrun.TargetPhaseScopeAllMaterializedTargets,
			wantNot: []string{
				"SELECT current_phase_run.current_phase_index",
				"MAX(phase_index)",
				"COALESCE(",
				"current_phase.status NOT IN",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sql := targetPhaseScopeSQL(t, tt.scope)
			for _, want := range tt.want {
				require.Contains(t, sql, want)
			}
			for _, wantNot := range tt.wantNot {
				require.NotContains(t, sql, wantNot)
			}
		})
	}
}

func targetPhaseScopeSQL(
	t *testing.T,
	scope operationrun.TargetPhaseScope,
) string {
	t.Helper()

	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	db := newOfflineBun()
	var rows []dbmodel.OperationRunTarget
	q := db.NewSelect().
		Model(&rows).
		Where("ort.operation_run_id = ?", runID)
	q = applyTargetPhaseScope(q, runID, scope)

	sql, err := q.AppendQuery(db.Formatter(), nil)
	require.NoError(t, err)
	return string(sql)
}

func TestFetchRunnableIDsRejectsNonPositiveLimit(t *testing.T) {
	store := &PostgresStore{}

	ids, err := store.FetchRunnableIDs(context.Background(), 0)

	require.Nil(t, ids)
	require.ErrorContains(t, err, "fetch runnable operation run limit must be greater than 0")
}

func TestRequireUpdatedRow(t *testing.T) {
	id := uuid.New()

	tests := []struct {
		name    string
		result  fakeSQLResult
		wantErr string
	}{
		{
			name:   "updated",
			result: fakeSQLResult{rowsAffected: 1},
		},
		{
			name:    "missing",
			result:  fakeSQLResult{},
			wantErr: "operation run target " + id.String() + " not found",
		},
		{
			name: "rows affected error",
			result: fakeSQLResult{
				err: errors.New("driver result failed"),
			},
			wantErr: "check operation run target " + id.String() + " update result: driver result failed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := requireUpdatedRow(tt.result, "operation run target", id)
			if tt.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}
