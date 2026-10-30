// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

func TestPostgresStore_ListEventHandlings(t *testing.T) {
	t.Run("wrapped errors", testListEventHandlingErrors)
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
		otherEventID, otherEventID.String(), componentID, ruleID, noActionJSON, handledAt, handledAt,
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
		rackID, ruleID, noActionJSON, handledAt, handledAt,
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
	t.Run("current inventory source identifiers", func(t *testing.T) {
		for _, source := range []struct {
			name       string
			table      string
			externalID string
			kind       eventrule.ResourceKind
			id         uuid.UUID
			total      int
		}{
			{name: "Rack", table: "rack", externalID: "rack-17", kind: eventrule.ResourceKindRack, id: rackID, total: 1},
			{name: "Component", table: "component", externalID: "component-17", kind: eventrule.ResourceKindComponent, id: componentID, total: 2},
		} {
			t.Run(source.name, func(t *testing.T) {
				filter := eventrule.HandlingQuery{EventType: leakage.TypeHardwareLeakDetected, ResourceType: source.kind, ResourceID: source.id, Limit: 10}
				original, total, err := store.ListEventHandlings(ctx, filter)
				require.NoError(t, err)
				require.Equal(t, source.total, total)
				require.Len(t, original, source.total)
				for _, handling := range original {
					require.Equal(t, source.externalID, handling.ResourceID)
				}
				t.Cleanup(func() {
					_, err := session.DB.ExecContext(ctx, "UPDATE ? SET external_id = ? WHERE id = ?", bun.Ident(source.table), source.externalID, source.id)
					require.NoError(t, err)
				})

				renamedID := source.externalID + "-renamed"
				for _, state := range []struct {
					name       string
					externalID *string
				}{
					{name: "renamed", externalID: &renamedID},
					{name: "no public identifier"},
				} {
					t.Run(state.name, func(t *testing.T) {
						_, err := session.DB.ExecContext(ctx, "UPDATE ? SET external_id = ? WHERE id = ?", bun.Ident(source.table), state.externalID, source.id)
						require.NoError(t, err)

						got, total, err := store.ListEventHandlings(ctx, filter)
						require.NoError(t, err)
						require.Equal(t, source.total, total)
						want := slices.Clone(original)
						for i := range want {
							want[i].ResourceID = ""
							if state.externalID != nil {
								want[i].ResourceID = *state.externalID
							}
						}
						// The captured UUID filter still selects the same events;
						// only the inventory display identifier changes.
						require.Equal(t, want, got)
					})
				}
			})
		}
	})
	t.Run("snapshot consistency", func(t *testing.T) {
		for _, tc := range []struct {
			name          string
			stage         string
			missingSource bool
		}{
			{name: "first handling commits between count and page", stage: "count", missingSource: true},
			{name: "newer handling commits between count and page", stage: "count"},
			{name: "execution and Task change between detail reads", stage: "executions"},
		} {
			t.Run(tc.name, func(t *testing.T) {
				requestCtx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
				defer cancel()
				sourceID := componentID
				if tc.missingSource {
					sourceID = uuid.New()
				}
				filter := eventrule.HandlingQuery{EventType: leakage.TypeHardwareLeakDetected, ResourceType: eventrule.ResourceKindComponent, ResourceID: sourceID, Limit: 1}
				before, beforeTotal, err := store.ListEventHandlings(requestCtx, filter)
				require.NoError(t, err)
				if tc.missingSource {
					require.Zero(t, beforeTotal)
					require.Empty(t, before)
				} else {
					require.Equal(t, 2, beforeTotal)
					require.Len(t, before, 1)
					require.Equal(t, latestEventID, before[0].ID)
				}

				newID := uuid.New()
				write := func() error {
					createdAt := handledAt.Add(24 * time.Hour)
					_, err := session.DB.ExecContext(requestCtx, `INSERT INTO events
						(id, source_name, source_key, event_type, resource_id, resource_kind, applied_rule_id, effective_policy, summary, observations, created_at, last_observed_at)
						VALUES (?, 'snapshot', ?, ?, ?, 'component', ?, ?, 'New leakage', 1, ?, ?)`,
						newID, newID.String(), leakage.TypeHardwareLeakDetected, sourceID, ruleID, noActionJSON, createdAt, createdAt)
					return err
				}
				if tc.stage == "executions" {
					setState := func(executionStatus eventrule.ExecutionStatus, taskStatus taskcommon.TaskStatus) error {
						return session.DB.RunInTx(ctx, nil, func(ctx context.Context, tx bun.Tx) error {
							_, err := tx.ExecContext(ctx, "UPDATE event_action_executions SET status = ? WHERE id = ?", executionStatus, latestExecutionID)
							if err != nil {
								return err
							}
							_, err = tx.ExecContext(ctx, "UPDATE task SET status = ? WHERE id = ?", taskStatus, latestTaskID)
							return err
						})
					}
					write = func() error {
						return setState(eventrule.ExecutionStatusCompleted, taskcommon.TaskStatusCompleted)
					}
					t.Cleanup(func() {
						require.NoError(t, setState(eventrule.ExecutionStatusFailed, taskcommon.TaskStatusRunning))
					})
				} else {
					t.Cleanup(func() {
						_, err := session.DB.ExecContext(ctx, "DELETE FROM events WHERE id = ?", newID)
						require.NoError(t, err)
					})
				}
				hook := &eventHandlingSnapshotHook{stage: tc.stage, write: write, done: make(chan struct{})}
				session.DB.AddQueryHook(hook)
				listCtx := context.WithValue(requestCtx, eventHandlingSnapshotContextKey{}, hook)
				rows, total, err := store.ListEventHandlings(listCtx, filter)
				require.NoError(t, err)
				select {
				case <-hook.done:
				default:
					t.Fatal("concurrent write must commit between the handling reads")
				}
				require.NoError(t, hook.err)
				require.Equal(t, beforeTotal, total)
				require.Equal(t, before, rows)

				next, nextTotal, err := store.ListEventHandlings(requestCtx, filter)
				require.NoError(t, err)
				require.Len(t, next, 1)
				if tc.stage == "count" {
					require.Equal(t, beforeTotal+1, nextTotal)
					require.Equal(t, newID, next[0].ID)
				} else {
					require.Equal(t, beforeTotal, nextTotal)
					require.Len(t, next[0].Executions, 1)
					require.Equal(t, eventrule.ExecutionStatusCompleted, next[0].Executions[0].Status)
					require.Len(t, next[0].Tasks, 1)
					require.Equal(t, taskcommon.TaskStatusCompleted, next[0].Tasks[0].Status)
				}
			})
		}
	})
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
			want: eventrule.Handling{AppliedRuleID: ruleID, ID: latestEventID, FirstObservedAt: detectedAt, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Component leakage", AppliedPolicy: shutdownPolicy, ResourceID: "component-17", ResourceType: eventrule.ResourceKindComponent, HandledAt: latestTime, Executions: []eventrule.HandlingExecution{{ID: latestExecutionID, ActionName: shutdownPolicy.Actions[0].Name, Status: eventrule.ExecutionStatusFailed}}, Tasks: []eventrule.HandlingTask{{ID: latestTaskID, ExecutionID: latestExecutionID, Status: taskcommon.TaskStatusRunning, CreatedAt: latestTime}}},
		},
		{
			name: "next page", eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: componentID, offset: 1, total: 2,
			want: eventrule.Handling{AppliedRuleID: ruleID, ID: olderEventID, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Component leakage", AppliedPolicy: shutdownPolicy, ResourceID: "component-17", ResourceType: eventrule.ResourceKindComponent, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{{ID: olderExecutionID, ActionName: shutdownPolicy.Actions[0].Name, Status: eventrule.ExecutionStatusCompleted}}, Tasks: []eventrule.HandlingTask{{ID: olderTaskID, ExecutionID: olderExecutionID, Status: taskcommon.TaskStatusCompleted, CreatedAt: handledAt}}},
		},
		{
			name: "rack snapshot without a live rule", total: 1, eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindRack, resourceID: rackID,
			want: eventrule.Handling{AppliedRuleID: ruleID, ID: rackEventID, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Rack leakage", AppliedPolicy: noActionPolicy, ResourceID: "rack-17", ResourceType: eventrule.ResourceKindRack, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{}, Tasks: []eventrule.HandlingTask{}},
		},
		{
			name: "other event family", total: 1, eventType: eventrule.Type("hardware.temperature.high"), kind: eventrule.ResourceKindComponent, resourceID: componentID,
			want: eventrule.Handling{AppliedRuleID: ruleID, ID: otherEventID, EventType: eventrule.Type("hardware.temperature.high"), EventSummary: "High temperature", AppliedPolicy: noActionPolicy, ResourceID: "component-17", ResourceType: eventrule.ResourceKindComponent, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{}, Tasks: []eventrule.HandlingTask{}},
		},
		{
			name: "deferred execution without a task", total: 1, eventType: leakage.TypeHardwareLeakDetected, kind: eventrule.ResourceKindComponent, resourceID: deferredSourceID,
			want: eventrule.Handling{AppliedRuleID: ruleID, ID: deferredEventID, EventType: leakage.TypeHardwareLeakDetected, EventSummary: "Deferred leakage", AppliedPolicy: shutdownPolicy, ResourceID: "component-deferred", ResourceType: eventrule.ResourceKindComponent, HandledAt: handledAt, Executions: []eventrule.HandlingExecution{{ID: deferredExecutionID, ActionName: shutdownPolicy.Actions[0].Name, Status: eventrule.ExecutionStatusDeferred, Reason: eventrule.ExecutionReasonAttemptFailed}}, Tasks: []eventrule.HandlingTask{}},
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
			got, total, err := store.ListEventHandlings(ctx, eventrule.HandlingQuery{EventType: test.eventType, ResourceType: test.kind, ResourceID: test.resourceID, Offset: test.offset, Limit: limit})
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

func TestPostgresStore_ListTasks(t *testing.T) {
	t.Run("wrapped errors", testListTaskErrors)
	if os.Getenv("DB_PORT") == "" {
		t.Skip("PostgreSQL environment required")
	}
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	session, err := utils.UnitTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	ctx := t.Context()
	rackID, otherRackID, policyID, operationRuleID := uuid.New(), uuid.New(), uuid.New(), uuid.New()
	trayID, otherTrayID := uuid.New(), uuid.New()
	_, err = session.DB.ExecContext(ctx, `INSERT INTO operation_rules(id,name,operation_type,operation_code,rule_definition) VALUES (?,'Task operation rule','power_control','4','{}')`, operationRuleID)
	require.NoError(t, err)
	snapshot, err := eventrulecodec.MarshalPolicy(leakage.DefaultRule().Policy)
	require.NoError(t, err)
	now := time.Now().UTC().Truncate(time.Microsecond)
	taskIDs := []uuid.UUID{uuid.New(), uuid.New(), uuid.New()}
	for i, taskID := range taskIDs {
		created := now.Add(time.Duration(i) * time.Minute)
		status := taskcommon.TaskStatusCompleted
		targetRack := rackID
		var triggerID *uuid.UUID
		triggerType := operation.TriggerTypeAPI
		if i < 2 {
			eventID, executionID := uuid.New(), uuid.New()
			triggerID = &executionID
			triggerType = operation.TriggerTypeEventRuleExecution
			// Both events came from rackID; the second Task targets another Rack.
			_, err = session.DB.ExecContext(ctx, `INSERT INTO events(id,source_name,source_key,event_type,resource_id,resource_kind,rack_id,applied_rule_id,effective_policy,summary,observations,created_at,last_observed_at) VALUES (?,'test',?,'hardware.leak.detected',?,'rack',?,?,?,'Captured',1,?,?)`, eventID, eventID.String(), rackID, rackID, policyID, snapshot, created, created)
			require.NoError(t, err)
			_, err = session.DB.ExecContext(ctx, `INSERT INTO event_action_executions(id,event_id,action_name,action_type,plan,status,attempts,created_at,updated_at) VALUES (?,?,'shutdown','submit_task','{}','completed',1,?,?)`, executionID, eventID, created, created)
			require.NoError(t, err)
		}
		if i == 1 {
			status = taskcommon.TaskStatusWaiting
			targetRack = otherRackID
		}
		targets := []uuid.UUID{trayID}
		if i == 0 {
			targets = append(targets, otherTrayID)
		}
		if i == 1 {
			targets = []uuid.UUID{otherTrayID}
		}
		attributes, marshalErr := json.Marshal(map[string]any{"components_by_type": map[string][]uuid.UUID{"Compute": targets}})
		require.NoError(t, marshalErr)
		// The Task operation rule deliberately differs from the captured Event policy.
		_, err = session.DB.ExecContext(ctx, `INSERT INTO task(id,type,executor_type,information,rack_id,execution_id,status,attributes,trigger_type,trigger_id,applied_rule_id,created_at,updated_at) VALUES (?,'power_control','temporal','{"operation":4}',?,?,?,?,?,?,?,?,?)`, taskID, targetRack, taskID.String(), status, string(attributes), triggerType, triggerID, operationRuleID, created, created)
		require.NoError(t, err)
	}
	store := NewPostgres(session)
	for _, tc := range []struct {
		name      string
		filter    *taskcommon.EventTaskFilter
		rack      uuid.UUID
		component uuid.UUID
		active    bool
		offset    int
		total     int32
		ids       []uuid.UUID
	}{
		{name: "ordinary and event Tasks", total: 3, ids: []uuid.UUID{taskIDs[2], taskIDs[1], taskIDs[0]}},
		{name: "target Rack alone includes ordinary Tasks", rack: rackID, total: 2, ids: []uuid.UUID{taskIDs[2], taskIDs[0]}},
		{name: "Tray alone includes ordinary Tasks", component: trayID, total: 2, ids: []uuid.UUID{taskIDs[2], taskIDs[0]}},
		{name: "Tray matches multi-target event Tasks", component: trayID, filter: &taskcommon.EventTaskFilter{PolicyID: policyID}, total: 1, ids: []uuid.UUID{taskIDs[0]}},
		{name: "Tray and active event filters combine", component: otherTrayID, filter: &taskcommon.EventTaskFilter{PolicyID: policyID}, active: true, total: 1, ids: []uuid.UUID{taskIDs[1]}},
		{name: "all event Tasks", filter: &taskcommon.EventTaskFilter{}, total: 2, ids: []uuid.UUID{taskIDs[1], taskIDs[0]}},
		{name: "captured Event policy without persisted definition", filter: &taskcommon.EventTaskFilter{PolicyID: policyID}, total: 2, ids: []uuid.UUID{taskIDs[1], taskIDs[0]}},
		{name: "event type and target Rack combine", filter: &taskcommon.EventTaskFilter{EventType: "hardware.leak.detected", PolicyID: policyID}, rack: otherRackID, total: 1, ids: []uuid.UUID{taskIDs[1]}},
		{name: "Rack selects target not captured source", filter: &taskcommon.EventTaskFilter{}, rack: rackID, total: 1, ids: []uuid.UUID{taskIDs[0]}},
		{name: "operation rule does not select Event policy", filter: &taskcommon.EventTaskFilter{PolicyID: operationRuleID}, total: 0},
		{name: "active includes waiting", filter: &taskcommon.EventTaskFilter{}, active: true, total: 1, ids: []uuid.UUID{taskIDs[1]}},
		{name: "active and target combine with AND", filter: &taskcommon.EventTaskFilter{}, rack: rackID, active: true, total: 0},
		{name: "out of range retains filtered total", filter: &taskcommon.EventTaskFilter{}, offset: 10, total: 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rows, total, err := store.ListTasks(ctx, &taskcommon.TaskListOptions{TaskType: taskcommon.TaskTypeUnknown, RackID: tc.rack, ComponentID: tc.component, ActiveOnly: tc.active, EventFilter: tc.filter}, &dbquery.Pagination{Offset: tc.offset, Limit: 10})
			require.NoError(t, err)
			require.Equal(t, tc.total, total)
			ids := make([]uuid.UUID, 0, len(rows))
			for _, row := range rows {
				ids = append(ids, row.ID)
			}
			if tc.ids == nil {
				require.Empty(t, ids)
			} else {
				require.Equal(t, tc.ids, ids)
			}
		})
	}
	t.Run("count and page share snapshot", func(t *testing.T) {
		newID := uuid.New()
		hook := &taskListSnapshotHook{afterCount: func() {
			_, insertErr := session.DB.ExecContext(ctx, `INSERT INTO task(id,type,executor_type,information,rack_id,execution_id,status,attributes,trigger_type,created_at,updated_at) VALUES (?,'power_control','temporal','{"operation":4}',?,?,'completed','{}',?,?,?)`, newID, rackID, newID.String(), operation.TriggerTypeAPI, now.Add(time.Hour), now.Add(time.Hour))
			require.NoError(t, insertErr)
		}}
		session.DB.AddQueryHook(hook)
		options := &taskcommon.TaskListOptions{TaskType: taskcommon.TaskTypeUnknown}
		rows, total, err := store.ListTasks(ctx, options, &dbquery.Pagination{Limit: 10})
		require.NoError(t, err)
		require.True(t, hook.fired)
		require.EqualValues(t, 3, total)
		require.Len(t, rows, 3)
		rows, total, err = store.ListTasks(ctx, options, &dbquery.Pagination{Limit: 10})
		require.NoError(t, err)
		require.EqualValues(t, 4, total)
		require.Len(t, rows, 4)
		require.Equal(t, newID, rows[0].ID)
	})
}

type eventHandlingSnapshotContextKey struct{}

type eventHandlingSnapshotHook struct {
	stage string
	write func() error
	done  chan struct{}
	err   error
}

func (h *eventHandlingSnapshotHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if ctx.Value(eventHandlingSnapshotContextKey{}) == h && h.stage == "count" &&
		strings.Contains(event.Query, "events AS e") && !strings.HasPrefix(event.Query, "SELECT count(*)") {
		// Bun can issue count and page reads concurrently. Gate the page until
		// the count and the independent writer have completed.
		select {
		case <-h.done:
		case <-ctx.Done():
		}
	}
	return ctx
}

func (h *eventHandlingSnapshotHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if ctx.Value(eventHandlingSnapshotContextKey{}) != h {
		return
	}
	isCount := strings.HasPrefix(event.Query, "SELECT count(*)") && strings.Contains(event.Query, "events AS e")
	isExecution := strings.Contains(event.Query, "eae.action_name")
	if (h.stage != "count" || !isCount) && (h.stage != "executions" || !isExecution) {
		return
	}
	defer close(h.done)
	h.err = event.Err
	if h.err == nil {
		h.err = h.write()
	}
}

type taskListSnapshotHook struct {
	afterCount func()
	fired      bool
}

func (*taskListSnapshotHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}
func (h *taskListSnapshotHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if h.fired || event.Err != nil || !strings.HasPrefix(event.Query, "SELECT count(*)") || !strings.Contains(event.Query, `"task"`) {
		return
	}
	h.fired = true
	h.afterCount()
}

func testListEventHandlingErrors(t *testing.T) {
	t.Helper()
	snapshot, err := eventrulecodec.MarshalPolicy(leakage.DefaultRule().Policy)
	require.NoError(t, err)
	for _, tc := range []struct {
		stage   string
		context string
	}{
		{stage: "begin", context: "list event handlings"},
		{stage: "count", context: "list event handlings"},
		{stage: "events", context: "list event handlings"},
		{stage: "policy", context: "decode applied event policy"},
		{stage: "executions", context: "list event handling executions"},
		{stage: "tasks", context: "list event handling tasks"},
		{stage: "commit", context: "list event handlings"},
	} {
		t.Run(tc.stage, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			db := bun.NewDB(sqlDB, pgdialect.New())
			defer db.Close()
			store := NewPostgres(&cdb.Session{DB: db})
			cause := &pgconn.PgError{Code: "08006", Message: "database connection failed"}
			begin := mock.ExpectBegin()
			switch tc.stage {
			case "begin":
				begin.WillReturnError(cause)
			case "count":
				mock.ExpectQuery(`SELECT count`).WillReturnError(cause)
				mock.ExpectRollback()
			case "commit":
				mock.ExpectQuery(`SELECT count`).WillReturnRows(sqlmock.NewRows([]string{"count"}).AddRow(0))
				mock.ExpectQuery(`SELECT .* FROM events AS e`).WillReturnRows(sqlmock.NewRows([]string{"id"}))
				mock.ExpectCommit().WillReturnError(cause)
			default:
				mock.ExpectQuery(`SELECT count`).WillReturnRows(sqlmock.NewRows([]string{"count"}).AddRow(1))
				query := mock.ExpectQuery(`SELECT .* FROM events AS e`)
				if tc.stage == "events" {
					query.WillReturnError(cause)
				} else {
					policy := snapshot
					if tc.stage == "policy" {
						policy = []byte("{")
					}
					query.WillReturnRows(sqlmock.NewRows([]string{"id", "effective_policy"}).AddRow(uuid.New(), policy))
					if tc.stage != "policy" {
						executions := mock.ExpectQuery(`SELECT .* FROM event_action_executions AS eae`)
						if tc.stage == "executions" {
							executions.WillReturnError(cause)
						} else {
							executions.WillReturnRows(sqlmock.NewRows([]string{"id"}))
							mock.ExpectQuery(`SELECT .* FROM event_action_executions AS eae`).WillReturnError(cause)
						}
					}
				}
				mock.ExpectRollback()
			}

			rows, total, err := store.ListEventHandlings(t.Context(), eventrule.HandlingQuery{})
			require.Nil(t, rows)
			require.Zero(t, total)
			require.ErrorContains(t, err, tc.context)
			_, grpcError := status.FromError(err)
			require.False(t, grpcError, "store errors must not carry a gRPC status")
			if tc.stage == "policy" {
				var syntaxError *json.SyntaxError
				require.ErrorAs(t, err, &syntaxError)
			} else {
				require.ErrorIs(t, err, cause)
				var pgError *pgconn.PgError
				require.ErrorAs(t, err, &pgError)
				require.Same(t, cause, pgError)
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}

func testListTaskErrors(t *testing.T) {
	t.Helper()
	for _, tc := range []struct {
		name       string
		conversion bool
	}{
		{name: "database failure"},
		{name: "invalid stored task", conversion: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			db := bun.NewDB(sqlDB, pgdialect.New())
			defer db.Close()
			store := NewPostgres(&cdb.Session{DB: db})
			cause := &pgconn.PgError{Code: "08006", Message: "database connection failed"}
			mock.ExpectBegin()
			mock.ExpectQuery(`SELECT count`).WillReturnRows(sqlmock.NewRows([]string{"count"}).AddRow(1))
			query := mock.ExpectQuery(`SELECT .* FROM "task" AS "t"`)
			if tc.conversion {
				query.WillReturnRows(sqlmock.NewRows([]string{"id", "trigger_type"}).AddRow(uuid.New(), "invalid"))
			} else {
				query.WillReturnError(cause)
			}

			if tc.conversion {
				mock.ExpectCommit()
			} else {
				mock.ExpectRollback()
			}
			rows, total, err := store.ListTasks(t.Context(), &taskcommon.TaskListOptions{TaskType: taskcommon.TaskTypeUnknown, EventFilter: &taskcommon.EventTaskFilter{}}, &dbquery.Pagination{Limit: 10})
			require.Nil(t, rows)
			require.Zero(t, total)
			require.Error(t, err)
			_, grpcError := status.FromError(err)
			require.False(t, grpcError, "store errors must not carry a gRPC status")
			if tc.conversion {
				require.ErrorContains(t, err, "decode task")
				require.NotNil(t, errors.Unwrap(err))
			} else {
				require.ErrorContains(t, err, "list tasks")
				require.ErrorIs(t, err, cause)
				var pgError *pgconn.PgError
				require.ErrorAs(t, err, &pgError)
				require.Same(t, cause, pgError)
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}
