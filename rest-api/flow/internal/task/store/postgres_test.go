// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"fmt"
	"os"
	"testing"
	"time"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

func TestPostgresStore_LatestLeakageShutdownTaskStatuses(t *testing.T) {
	t.Run("empty component set does not query", func(t *testing.T) {
		store := &PostgresStore{}
		got, err := store.LatestLeakageShutdownTaskStatuses(t.Context(), nil)
		require.NoError(t, err)
		assert.Empty(t, got)
	})

	t.Run("returns one latest status per component", func(t *testing.T) {
		sqlDB, mock, err := sqlmock.New()
		require.NoError(t, err)
		defer sqlDB.Close()

		db := bun.NewDB(sqlDB, pgdialect.New())
		defer db.Close()
		store := &PostgresStore{pg: &cdb.Session{DB: db}}
		componentA := uuid.New()
		componentB := uuid.New()

		mock.ExpectQuery(`SELECT DISTINCT ON \(target.component_id\).*FROM task AS t`).
			WillReturnRows(sqlmock.NewRows([]string{"component_id", "status"}).
				AddRow(componentA, taskcommon.TaskStatusRunning).
				AddRow(componentB, taskcommon.TaskStatusCompleted))

		got, err := store.LatestLeakageShutdownTaskStatuses(
			t.Context(),
			[]uuid.UUID{componentB, uuid.Nil, componentA, componentA},
		)

		require.NoError(t, err)
		assert.Equal(t, map[uuid.UUID]taskcommon.TaskStatus{
			componentA: taskcommon.TaskStatusRunning,
			componentB: taskcommon.TaskStatusCompleted,
		}, got)
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("filters and selects latest task in postgres", func(t *testing.T) {
		if os.Getenv("DB_PORT") == "" {
			t.Skip("Skipping PostgreSQL task store test: no DB environment specified")
		}

		config, err := cdb.ConfigFromEnv()
		require.NoError(t, err)
		session, err := utils.UnitTestDB(context.Background(), t, config)
		require.NoError(t, err)
		t.Cleanup(session.Close)

		ctx := t.Context()
		componentA := uuid.New()
		componentB := uuid.New()
		rackID := uuid.New()
		leakEventID := uuid.New()
		otherEventID := uuid.New()
		leakExecutionID := uuid.New()
		otherExecutionID := uuid.New()
		now := time.Now().UTC()

		for _, event := range []struct {
			id        uuid.UUID
			eventType string
		}{
			{id: leakEventID, eventType: "hardware.leak.detected"},
			{id: otherEventID, eventType: "hardware.test.detected"},
		} {
			_, err = session.DB.ExecContext(ctx, `
			INSERT INTO events (
				id, source_name, source_key, event_type, resource_id,
				resource_kind, applied_rule_id, effective_policy, summary,
				observations, created_at, last_observed_at
			) VALUES (?, 'test', ?, ?, ?, 'component', ?, '{}', '', 1, ?, ?)`,
				event.id, event.id.String(), event.eventType, componentA, uuid.New(), now, now,
			)
			require.NoError(t, err)
		}

		for _, execution := range []struct {
			id      uuid.UUID
			eventID uuid.UUID
		}{
			{id: leakExecutionID, eventID: leakEventID},
			{id: otherExecutionID, eventID: otherEventID},
		} {
			_, err = session.DB.ExecContext(ctx, `
			INSERT INTO event_action_executions (
				id, event_id, action_name, action_type, plan, status,
				attempts, created_at, updated_at
			) VALUES (?, ?, 'shutdown', 'submit_task', '{}', 'completed', 1, ?, ?)`,
				execution.id, execution.eventID, now, now,
			)
			require.NoError(t, err)
		}

		type taskFixture struct {
			componentID uuid.UUID
			triggerID   uuid.UUID
			operation   operations.PowerOperation
			status      taskcommon.TaskStatus
			createdAt   time.Time
		}
		fixtures := []taskFixture{
			// The newest matching task wins for component A.
			{componentA, leakExecutionID, operations.PowerOperationForcePowerOff, taskcommon.TaskStatusFailed, now.Add(-time.Minute)},
			{componentA, leakExecutionID, operations.PowerOperationForcePowerOff, taskcommon.TaskStatusRunning, now},
			{componentB, leakExecutionID, operations.PowerOperationForcePowerOff, taskcommon.TaskStatusCompleted, now},
			// A different event type and operation must not affect component A.
			{componentA, otherExecutionID, operations.PowerOperationForcePowerOff, taskcommon.TaskStatusCompleted, now.Add(time.Minute)},
			{componentA, leakExecutionID, operations.PowerOperationPowerOn, taskcommon.TaskStatusCompleted, now.Add(2 * time.Minute)},
		}
		for i, fixture := range fixtures {
			attributes := fmt.Sprintf(`{"components_by_type":{"compute":["%s"]}}`, fixture.componentID)
			information := fmt.Sprintf(`{"operation":%d}`, fixture.operation)
			_, err = session.DB.ExecContext(ctx, `
			INSERT INTO task (
				id, type, executor_type, information, rack_id, execution_id,
				status, attributes, created_at, updated_at, trigger_type, trigger_id
			) VALUES (?, ?, 'temporal', ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
				uuid.New(), taskcommon.TaskTypePowerControl, information, rackID,
				fmt.Sprintf("execution-%d", i), fixture.status, attributes,
				fixture.createdAt, fixture.createdAt,
				operation.TriggerTypeEventRuleExecution, fixture.triggerID,
			)
			require.NoError(t, err)
		}

		got, err := NewPostgres(session).LatestLeakageShutdownTaskStatuses(
			ctx,
			[]uuid.UUID{componentA, componentB, uuid.New()},
		)
		require.NoError(t, err)
		assert.Equal(t, map[uuid.UUID]taskcommon.TaskStatus{
			componentA: taskcommon.TaskStatusRunning,
			componentB: taskcommon.TaskStatusCompleted,
		}, got)
	})
}

func TestTaskComponentsAnyPredicate(t *testing.T) {
	componentA := uuid.MustParse("11111111-2222-3333-4444-555555555555")
	componentB := uuid.MustParse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")

	got := taskComponentsAnyPredicate([]uuid.UUID{componentA, componentB})

	assert.Equal(t,
		`(t.attributes @? '$.components_by_type.*[*] ? (@ == "11111111-2222-3333-4444-555555555555")'::jsonpath OR t.attributes @? '$.components_by_type.*[*] ? (@ == "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")'::jsonpath)`,
		got,
	)
}
