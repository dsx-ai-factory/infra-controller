// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"fmt"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

func TestPostgresStore_GetEventHandling(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping PostgreSQL task store test: no DB environment specified")
	}

	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(context.Background(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)

	ctx := t.Context()
	componentID := uuid.New()
	olderEventID, latestEventID, rackEventID, otherEventID := uuid.New(), uuid.New(), uuid.New(), uuid.New()
	olderExecutionID, latestExecutionID := uuid.New(), uuid.New()
	olderTaskID, latestTaskID := uuid.New(), uuid.New()
	rackID := uuid.New()
	handledAt := time.Now().UTC().Add(-time.Hour).Truncate(time.Microsecond)

	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO component (id, external_id, name, type, leak_status)
		VALUES (?, 'component-17', 'component 17', 'Compute', 'DETECTED')`, componentID)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO events (
			id, source_name, source_key, event_type, resource_id,
			resource_kind, applied_rule_id, effective_policy, summary,
			observations, created_at, last_observed_at
		) VALUES (?, 'test', ?, 'hardware.temperature.high', ?, 'component', ?, '{}', '', 1, ?, ?)`,
		otherEventID, otherEventID.String(), componentID, uuid.New(), handledAt, handledAt,
	)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO rack (id, external_id, name)
		VALUES (?, 'rack-17', 'rack 17')`, rackID)
	require.NoError(t, err)
	for index, eventID := range []uuid.UUID{olderEventID, latestEventID} {
		createdAt := handledAt.Add(time.Duration(index) * time.Minute)
		_, err = session.DB.ExecContext(ctx, `
			INSERT INTO events (
				id, source_name, source_key, event_type, resource_id,
				resource_kind, applied_rule_id, effective_policy, summary,
				observations, created_at, last_observed_at
			) VALUES (?, 'test', ?, ?, ?, 'component', ?, '{}', '', ?, ?, ?)`,
			eventID, eventID.String(), leakage.TypeHardwareLeakDetected,
			componentID, uuid.New(), index+1, createdAt, createdAt,
		)
		require.NoError(t, err)
	}
	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO events (
			id, source_name, source_key, event_type, resource_id,
			resource_kind, applied_rule_id, effective_policy, summary,
			observations, created_at, last_observed_at
		) VALUES (?, 'test', ?, ?, ?, 'rack', ?, '{}', '', 1, ?, ?)`,
		rackEventID, rackEventID.String(), leakage.TypeHardwareLeakDetected,
		rackID, uuid.New(), handledAt, handledAt,
	)
	require.NoError(t, err)

	for _, fixture := range []struct {
		eventID     uuid.UUID
		executionID uuid.UUID
		taskID      uuid.UUID
		status      taskcommon.TaskStatus
		createdAt   time.Time
	}{
		{olderEventID, olderExecutionID, olderTaskID, taskcommon.TaskStatusCompleted, handledAt},
		{latestEventID, latestExecutionID, latestTaskID, taskcommon.TaskStatusRunning, handledAt.Add(time.Minute)},
	} {
		_, err = session.DB.ExecContext(ctx, `
			INSERT INTO event_action_executions (
				id, event_id, action_name, action_type, plan, status,
				attempts, created_at, updated_at
			) VALUES (?, ?, 'shutdown', 'submit_task', '{}', 'completed', 1, ?, ?)`,
			fixture.executionID, fixture.eventID, fixture.createdAt, fixture.createdAt,
		)
		require.NoError(t, err)
		information := fmt.Sprintf(`{"operation":%d}`, operations.PowerOperationForcePowerOff)
		_, err = session.DB.ExecContext(ctx, `
			INSERT INTO task (
				id, type, executor_type, information, rack_id, execution_id,
				status, attributes, created_at, updated_at, trigger_type, trigger_id
			) VALUES (?, ?, 'temporal', ?, ?, ?, ?, '{}', ?, ?, ?, ?)`,
			fixture.taskID, taskcommon.TaskTypePowerControl, information, rackID,
			fixture.taskID.String(), fixture.status, fixture.createdAt, fixture.createdAt,
			operation.TriggerTypeEventRuleExecution, fixture.executionID,
		)
		require.NoError(t, err)
	}

	store := NewPostgres(session)
	latest, hasMore, err := store.GetEventHandling(ctx, leakage.TypeHardwareLeakDetected, eventrule.ResourceKindComponent, componentID, 1, nil)
	require.NoError(t, err)
	require.True(t, hasMore)
	require.Len(t, latest, 1)
	require.Equal(t, latestEventID, latest[0].ID)
	require.Equal(t, leakage.TypeHardwareLeakDetected, latest[0].EventType)
	require.Equal(t, "component-17", latest[0].ResourceID)
	require.Equal(t, eventrule.ResourceKindComponent, latest[0].ResourceType)
	require.Len(t, latest[0].Tasks, 1)
	require.Equal(t, latestTaskID, latest[0].Tasks[0].ID)
	require.Equal(t, taskcommon.TaskStatusRunning, latest[0].Tasks[0].Status)
	require.True(t, handledAt.Add(time.Minute).Equal(latest[0].Tasks[0].CreatedAt))

	cursor := &eventrule.HandlingCursor{HandledAt: latest[0].HandledAt, ID: latest[0].ID}
	next, hasMore, err := store.GetEventHandling(ctx, leakage.TypeHardwareLeakDetected, eventrule.ResourceKindComponent, componentID, 1, cursor)
	require.NoError(t, err)
	require.False(t, hasMore)
	require.Len(t, next, 1)
	require.Equal(t, olderEventID, next[0].ID)
	require.Len(t, next[0].Tasks, 1)
	require.Equal(t, olderTaskID, next[0].Tasks[0].ID)
	require.Equal(t, taskcommon.TaskStatusCompleted, next[0].Tasks[0].Status)
	require.True(t, handledAt.Equal(next[0].Tasks[0].CreatedAt))

	rackHandling, hasMore, err := store.GetEventHandling(ctx, leakage.TypeHardwareLeakDetected, eventrule.ResourceKindRack, rackID, 1, nil)
	require.NoError(t, err)
	require.False(t, hasMore)
	require.Len(t, rackHandling, 1)
	require.Equal(t, rackEventID, rackHandling[0].ID)
	require.Equal(t, "rack-17", rackHandling[0].ResourceID)
	require.Equal(t, eventrule.ResourceKindRack, rackHandling[0].ResourceType)
	require.Empty(t, rackHandling[0].Tasks)

	otherHandling, hasMore, err := store.GetEventHandling(ctx, eventrule.Type("hardware.temperature.high"), eventrule.ResourceKindComponent, componentID, 1, nil)
	require.NoError(t, err)
	require.False(t, hasMore)
	require.Len(t, otherHandling, 1)
	require.Equal(t, otherEventID, otherHandling[0].ID)
	require.Equal(t, eventrule.Type("hardware.temperature.high"), otherHandling[0].EventType)
}
