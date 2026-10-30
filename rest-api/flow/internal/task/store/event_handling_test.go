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
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
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
	shutdownPolicy := leakage.DefaultRule().Policy
	noActionPolicy := eventrule.Policy{Actions: []eventrule.Action{{Name: "record", Spec: &eventrule.Noop{Reason: "no automatic shutdown"}}}}
	shutdownJSON, err := eventrulecodec.MarshalPolicy(shutdownPolicy)
	require.NoError(t, err)
	noActionJSON, err := eventrulecodec.MarshalPolicy(noActionPolicy)
	require.NoError(t, err)
	ruleID := uuid.New()
	// The current rule differs from the snapshots recorded on component events.
	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO event_rules (id, name, description, enabled, event_type, policy)
		VALUES (?, 'Changed leakage response', '', true, ?, ?)`, ruleID, leakage.TypeHardwareLeakDetected, noActionJSON)
	require.NoError(t, err)

	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO component (id, external_id, name, type, leak_status)
		VALUES (?, 'component-17', 'component 17', 'Compute', 'DETECTED')`, componentID)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO events (
			id, source_name, source_key, event_type, resource_id,
			resource_kind, applied_rule_id, effective_policy, summary,
			observations, created_at, last_observed_at
		) VALUES (?, 'test', ?, 'hardware.temperature.high', ?, 'component', ?, ?, 'High temperature', 1, ?, ?)`,
		otherEventID, otherEventID.String(), componentID, uuid.New(), noActionJSON, handledAt, handledAt,
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
			) VALUES (?, 'test', ?, ?, ?, 'component', ?, ?, 'Component leakage', ?, ?, ?)`,
			eventID, eventID.String(), leakage.TypeHardwareLeakDetected,
			componentID, ruleID, shutdownJSON, index+1, createdAt, createdAt,
		)
		require.NoError(t, err)
	}
	_, err = session.DB.ExecContext(ctx, `
		INSERT INTO events (
			id, source_name, source_key, event_type, resource_id,
			resource_kind, applied_rule_id, effective_policy, summary,
			observations, created_at, last_observed_at
		) VALUES (?, 'test', ?, ?, ?, 'rack', ?, ?, 'Rack leakage', 1, ?, ?)`,
		rackEventID, rackEventID.String(), leakage.TypeHardwareLeakDetected,
		rackID, uuid.New(), noActionJSON, handledAt, handledAt,
	)
	require.NoError(t, err)

	for _, fixture := range []struct {
		eventID         uuid.UUID
		executionID     uuid.UUID
		taskID          uuid.UUID
		status          taskcommon.TaskStatus
		executionStatus eventrule.ExecutionStatus
		attempts        int
		createdAt       time.Time
	}{
		{olderEventID, olderExecutionID, olderTaskID, taskcommon.TaskStatusCompleted, eventrule.ExecutionStatusCompleted, 1, handledAt},
		{latestEventID, latestExecutionID, latestTaskID, taskcommon.TaskStatusRunning, eventrule.ExecutionStatusFailed, 4, handledAt.Add(time.Minute)},
	} {
		_, err = session.DB.ExecContext(ctx, `
			INSERT INTO event_action_executions (
				id, event_id, action_name, action_type, plan, status,
				attempts, created_at, updated_at, status_message
			) VALUES (?, ?, ?, 'submit_task', '{}', ?, ?, ?, ?, 'private backend diagnostic')`,
			fixture.executionID, fixture.eventID, shutdownPolicy.Actions[0].Name, fixture.executionStatus, fixture.attempts, fixture.createdAt, fixture.createdAt,
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

	deferredSourceID, deferredEventID, deferredExecutionID := uuid.New(), uuid.New(), uuid.New()
	retryAt := handledAt.Add(2 * time.Hour)
	_, err = session.DB.ExecContext(ctx, `INSERT INTO component (id, external_id, name, type, leak_status)
		VALUES (?, 'component-deferred', 'component deferred', 'Compute', 'DETECTED')`, deferredSourceID)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, `INSERT INTO events (
		id, source_name, source_key, event_type, resource_id, resource_kind, applied_rule_id,
		effective_policy, summary, observations, created_at, last_observed_at
	) VALUES (?, 'test', ?, ?, ?, 'component', ?, ?, 'Deferred leakage', 1, ?, ?)`,
		deferredEventID, deferredEventID.String(), leakage.TypeHardwareLeakDetected, deferredSourceID,
		ruleID, shutdownJSON, handledAt, handledAt)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, `INSERT INTO event_action_executions (
		id, event_id, action_name, action_type, plan, status, reason, attempts,
		created_at, updated_at, next_attempt_at, status_message
	) VALUES (?, ?, ?, 'submit_task', '{}', 'deferred', 'attempt_failed', 2, ?, ?, ?, 'private backend diagnostic')`,
		deferredExecutionID, deferredEventID, shutdownPolicy.Actions[0].Name, handledAt, handledAt, retryAt)
	require.NoError(t, err)

	detectedAt := handledAt.Add(-time.Minute)
	_, err = session.DB.ExecContext(ctx, "UPDATE events SET first_observed_at = ? WHERE id = ?", detectedAt, latestEventID)
	require.NoError(t, err)
	store := NewPostgres(session)
	latestTime := handledAt.Add(time.Minute)
	tests := []struct {
		name          string
		eventType     eventrule.Type
		kind          eventrule.ResourceKind
		resourceID    uuid.UUID
		offset        int
		want          eventrule.Handling
		total         int
		corruptPolicy bool
		empty         bool
		limit         int
		wantIDs       []uuid.UUID
	}{
		{
			name: "latest uses captured policy despite rule edit", eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: componentID, total: 2,
			want: eventrule.Handling{ID: latestEventID, FirstObservedAt: detectedAt, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Component leakage", AppliedPolicy: shutdownPolicy, ResourceID: "component-17", ResourceType: eventrule.ResourceKindComponent, HandledAt: latestTime, Executions: []eventrule.HandlingExecution{{ID: latestExecutionID, ActionName: shutdownPolicy.Actions[0].Name, Status: eventrule.ExecutionStatusFailed}}, Tasks: []eventrule.HandlingTask{{ID: latestTaskID, ExecutionID: latestExecutionID, Status: taskcommon.TaskStatusRunning, CreatedAt: latestTime}}},
		},
		{
			name: "next page", eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: componentID, offset: 1, total: 2,
			want: eventrule.Handling{ID: olderEventID, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Component leakage", AppliedPolicy: shutdownPolicy, ResourceID: "component-17", ResourceType: eventrule.ResourceKindComponent, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{{ID: olderExecutionID, ActionName: shutdownPolicy.Actions[0].Name, Status: eventrule.ExecutionStatusCompleted}}, Tasks: []eventrule.HandlingTask{{ID: olderTaskID, ExecutionID: olderExecutionID, Status: taskcommon.TaskStatusCompleted, CreatedAt: handledAt}}},
		},
		{
			name: "rack snapshot without a live rule", total: 1, eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindRack, resourceID: rackID,
			want: eventrule.Handling{ID: rackEventID, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Rack leakage", AppliedPolicy: noActionPolicy, ResourceID: "rack-17", ResourceType: eventrule.ResourceKindRack, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{}, Tasks: []eventrule.HandlingTask{}},
		},
		{
			name: "other event family", total: 1, eventType: eventrule.Type("hardware.temperature.high"), kind: eventrule.ResourceKindComponent, resourceID: componentID,
			want: eventrule.Handling{ID: otherEventID, EventType: eventrule.Type("hardware.temperature.high"), EventSummary: "High temperature", AppliedPolicy: noActionPolicy, ResourceID: "component-17", ResourceType: eventrule.ResourceKindComponent, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{}, Tasks: []eventrule.HandlingTask{}},
		},
		{
			name: "deferred execution without a task", total: 1, eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: deferredSourceID,
			want: eventrule.Handling{ID: deferredEventID, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Deferred leakage", AppliedPolicy: shutdownPolicy, ResourceID: "component-deferred", ResourceType: eventrule.ResourceKindComponent, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{{ID: deferredExecutionID, ActionName: shutdownPolicy.Actions[0].Name, Status: eventrule.ExecutionStatusDeferred, Reason: eventrule.ExecutionReasonAttemptFailed}}, Tasks: []eventrule.HandlingTask{}},
		},
		{name: "full page is ordered before pagination", eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: componentID, limit: 2, total: 2, wantIDs: []uuid.UUID{latestEventID, olderEventID}},
		{name: "offset beyond history retains total", eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: componentID, offset: 20, total: 2, empty: true},
		{name: "invalid persisted snapshot", eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: componentID, corruptPolicy: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.corruptPolicy {
				_, err := session.DB.ExecContext(ctx, "UPDATE events SET effective_policy = '{}' WHERE id = ?", latestEventID)
				require.NoError(t, err)
			}
			limit := test.limit
			if limit == 0 {
				limit = 1
			}
			got, total, err := store.GetEventHandling(ctx, test.eventType, test.kind, test.resourceID, test.offset, limit)
			if test.corruptPolicy {
				require.ErrorContains(t, err, "decode applied event policy")
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.total, total)
			if test.wantIDs != nil {
				ids := make([]uuid.UUID, 0, len(got))
				for _, item := range got {
					ids = append(ids, item.ID)
				}
				require.Equal(t, test.wantIDs, ids)
				return
			}
			if test.empty {
				require.Empty(t, got)
				return
			}
			require.Len(t, got, 1)
			require.Equal(t, test.want, got[0])
		})
	}
}
