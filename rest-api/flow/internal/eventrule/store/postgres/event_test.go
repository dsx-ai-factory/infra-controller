// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

func TestObserveEvent(t *testing.T) {
	ctx := t.Context()
	store := newTestStore(t)
	store.pg.DB.SetMaxOpenConns(1)
	store.pg.DB.SetMaxIdleConns(1)
	createdAt := time.Date(2026, 9, 1, 12, 0, 0, 0, time.UTC)
	store.timestamps = fixedTimestampSource{now: &createdAt}
	definition := testEventDefinition()
	created, err := store.CommitEventPlan(ctx, definition, testEventPlan())
	require.NoError(t, err)
	require.NotNil(t, created)
	executions, err := store.Executions(ctx)
	require.NoError(t, err)
	require.Len(t, executions, 1)
	observedAt := createdAt.Add(time.Minute)
	var backendPID int
	err = store.pg.DB.QueryRowContext(ctx, "SELECT pg_backend_pid()").Scan(&backendPID)
	require.NoError(t, err)
	var preparedName string
	for _, phase := range []struct {
		name         string
		addColumn    bool
		observations int
	}{
		{name: "before column addition", observations: 2},
		{name: "after column addition", addColumn: true, observations: 3},
	} {
		t.Run(phase.name, func(t *testing.T) {
			if phase.addColumn {
				_, err = store.pg.DB.ExecContext(ctx, "ALTER TABLE events ADD COLUMN test_added_column text")
				require.NoError(t, err)
			}
			// Keep the timestamp argument fixed so both calls use the same SQL.
			observed, err := observeEvent(ctx, store.pg.DB, definition.Key, observedAt)
			require.NoError(t, err)
			expected := *created
			expected.Observations = phase.observations
			expected.LastObservedAt = observedAt
			require.Equal(t, &expected, observed)
			storedEvents, err := store.Events(ctx)
			require.NoError(t, err)
			require.Equal(t, []eventrule.Event{*observed}, storedEvents)
			storedExecutions, err := store.Executions(ctx)
			require.NoError(t, err)
			require.Equal(t, executions, storedExecutions)

			var currentPID int
			err = store.pg.DB.QueryRowContext(ctx, "SELECT pg_backend_pid()").Scan(&currentPID)
			require.NoError(t, err)
			require.Equal(t, backendPID, currentPID)
			var currentPreparedName string
			err = store.pg.DB.QueryRowContext(ctx, `SELECT name FROM pg_prepared_statements
				WHERE statement LIKE 'UPDATE "events" AS "e" %'
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
