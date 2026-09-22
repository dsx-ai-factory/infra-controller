// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package taskschedule

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// ─── mock implementations ─────────────────────────────────────────────────────

// mockScheduleStore implements Store for use in dispatcher unit tests.
// Fields are function values; unset fields panic so tests catch unexpected calls.
// RunInTransaction defaults to calling fn(ctx) inline (no transaction semantics).
type mockScheduleStore struct {
	runInTransactionFn       func(ctx context.Context, fn func(context.Context) error) error
	fetchDueFn               func(ctx context.Context, batchSize int, now time.Time) ([]uuid.UUID, error)
	lockForFireFn            func(ctx context.Context, id uuid.UUID, now time.Time) (*dbmodel.TaskSchedule, error)
	lockForTriggerFn         func(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error)
	listScopesFn             func(ctx context.Context, scheduleID uuid.UUID) ([]*dbmodel.TaskScheduleScope, error)
	updateAfterFireFn        func(ctx context.Context, id uuid.UUID, u AfterFireUpdate) error
	updateFn                 func(ctx context.Context, id uuid.UUID, fields UpdateFields) (*dbmodel.TaskSchedule, error)
	updateScopeLastTaskIDsFn func(ctx context.Context, scopeTaskIDs map[uuid.UUID]uuid.UUID) error
}

func (m *mockScheduleStore) RunInTransaction(ctx context.Context, fn func(context.Context) error) error {
	if m.runInTransactionFn != nil {
		return m.runInTransactionFn(ctx, fn)
	}
	return fn(ctx)
}

func (m *mockScheduleStore) UpdateAfterFire(ctx context.Context, id uuid.UUID, u AfterFireUpdate) error {
	if m.updateAfterFireFn != nil {
		return m.updateAfterFireFn(ctx, id, u)
	}
	return nil
}

func (m *mockScheduleStore) Update(ctx context.Context, id uuid.UUID, f UpdateFields) (*dbmodel.TaskSchedule, error) {
	if m.updateFn != nil {
		return m.updateFn(ctx, id, f)
	}
	return &dbmodel.TaskSchedule{ID: id}, nil
}

func (m *mockScheduleStore) LockForFire(ctx context.Context, id uuid.UUID, now time.Time) (*dbmodel.TaskSchedule, error) {
	if m.lockForFireFn != nil {
		return m.lockForFireFn(ctx, id, now)
	}
	panic("mockScheduleStore.LockForFire: not set")
}

func (m *mockScheduleStore) LockForTrigger(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error) {
	if m.lockForTriggerFn != nil {
		return m.lockForTriggerFn(ctx, id)
	}
	panic("mockScheduleStore.LockForTrigger: not set")
}

func (m *mockScheduleStore) ListScopes(ctx context.Context, scheduleID uuid.UUID) ([]*dbmodel.TaskScheduleScope, error) {
	if m.listScopesFn != nil {
		return m.listScopesFn(ctx, scheduleID)
	}
	panic("mockScheduleStore.ListScopes: not set")
}

func (m *mockScheduleStore) UpdateScopeLastTaskIDs(ctx context.Context, scopeTaskIDs map[uuid.UUID]uuid.UUID) error {
	if m.updateScopeLastTaskIDsFn != nil {
		return m.updateScopeLastTaskIDsFn(ctx, scopeTaskIDs)
	}
	return nil
}

func (m *mockScheduleStore) FetchDue(ctx context.Context, batchSize int, now time.Time) ([]uuid.UUID, error) {
	if m.fetchDueFn != nil {
		return m.fetchDueFn(ctx, batchSize, now)
	}
	panic("mockScheduleStore.FetchDue: not set")
}

func (m *mockScheduleStore) Create(_ context.Context, _ *dbmodel.TaskSchedule) (uuid.UUID, error) {
	panic("mockScheduleStore.Create: not implemented")
}

func (m *mockScheduleStore) Get(_ context.Context, _ uuid.UUID) (*dbmodel.TaskSchedule, error) {
	panic("mockScheduleStore.Get: not implemented")
}

func (m *mockScheduleStore) List(_ context.Context, _ ListOptions) ([]*dbmodel.TaskSchedule, int32, error) {
	panic("mockScheduleStore.List: not implemented")
}

func (m *mockScheduleStore) SetEnabled(_ context.Context, _ uuid.UUID, _ bool) (*dbmodel.TaskSchedule, error) {
	panic("mockScheduleStore.SetEnabled: not implemented")
}

func (m *mockScheduleStore) Resume(_ context.Context, _ uuid.UUID, _ *time.Time) (*dbmodel.TaskSchedule, error) {
	panic("mockScheduleStore.Resume: not implemented")
}

func (m *mockScheduleStore) Delete(_ context.Context, _ uuid.UUID) error {
	panic("mockScheduleStore.Delete: not implemented")
}

func (m *mockScheduleStore) CreateScopes(_ context.Context, _ []*dbmodel.TaskScheduleScope) error {
	panic("mockScheduleStore.CreateScopes: not implemented")
}

func (m *mockScheduleStore) GetScope(_ context.Context, _ uuid.UUID) (*dbmodel.TaskScheduleScope, error) {
	panic("mockScheduleStore.GetScope: not implemented")
}

func (m *mockScheduleStore) DeleteScope(_ context.Context, _ uuid.UUID) error {
	panic("mockScheduleStore.DeleteScope: not implemented")
}

func (m *mockScheduleStore) UpdateScopeComponentFilter(_ context.Context, _ uuid.UUID, _ json.RawMessage) error {
	panic("mockScheduleStore.UpdateScopeComponentFilter: not implemented")
}

// mockTaskStore implements taskstore.Store. Only GetTask is functional; all
// other methods panic to catch unexpected calls.
type mockTaskStore struct {
	getTaskFn func(ctx context.Context, id uuid.UUID) (*taskdef.Task, error)
}

func (m *mockTaskStore) GetTask(ctx context.Context, id uuid.UUID) (*taskdef.Task, error) {
	if m.getTaskFn != nil {
		return m.getTaskFn(ctx, id)
	}
	panic("mockTaskStore.GetTask: not set")
}

func (m *mockTaskStore) RunInTransaction(ctx context.Context, fn func(context.Context) error) error {
	return fn(ctx)
}

func (m *mockTaskStore) CreateTask(_ context.Context, _ *taskdef.Task) error {
	panic("mockTaskStore.CreateTask: not implemented")
}

func (m *mockTaskStore) LockRack(_ context.Context, _ uuid.UUID) error {
	panic("mockTaskStore.LockRack: not implemented")
}

func (m *mockTaskStore) LockIdempotencyKey(_ context.Context, _ string) error {
	panic("mockTaskStore.LockIdempotencyKey: not implemented")
}

func (m *mockTaskStore) GetTaskByIdempotencyKey(_ context.Context, _ string) (*taskdef.Task, error) {
	panic("mockTaskStore.GetTaskByIdempotencyKey: not implemented")
}

func (m *mockTaskStore) GetTasks(_ context.Context, _ []uuid.UUID) ([]*taskdef.Task, error) {
	panic("mockTaskStore.GetTasks: not implemented")
}

func (m *mockTaskStore) ListTasks(_ context.Context, _ *taskcommon.TaskListOptions, _ *dbquery.Pagination) ([]*taskdef.Task, int32, error) {
	panic("mockTaskStore.ListTasks: not implemented")
}

func (m *mockTaskStore) ListNonTerminalTasksForRacks(_ context.Context, _ []uuid.UUID) ([]*taskdef.Task, error) {
	panic("mockTaskStore.ListNonTerminalTasksForRacks: not implemented")
}

func (m *mockTaskStore) UpdateScheduledTask(_ context.Context, _ *taskdef.Task) error {
	panic("mockTaskStore.UpdateScheduledTask: not implemented")
}

func (m *mockTaskStore) UpdateTaskStatus(_ context.Context, _ *taskdef.TaskStatusUpdate) error {
	panic("mockTaskStore.UpdateTaskStatus: not implemented")
}

func (m *mockTaskStore) UpdateTaskReport(_ context.Context, _ *taskdef.TaskReportUpdate) error {
	panic("mockTaskStore.UpdateTaskReport: not implemented")
}

func (m *mockTaskStore) ListActiveTasksForRack(_ context.Context, _ uuid.UUID) ([]*taskdef.Task, error) {
	panic("mockTaskStore.ListActiveTasksForRack: not implemented")
}

func (m *mockTaskStore) ListWaitingTasksForRack(_ context.Context, _ uuid.UUID) ([]*taskdef.Task, error) {
	panic("mockTaskStore.ListWaitingTasksForRack: not implemented")
}

func (m *mockTaskStore) CountWaitingTasksForRack(_ context.Context, _ uuid.UUID) (int, error) {
	panic("mockTaskStore.CountWaitingTasksForRack: not implemented")
}

func (m *mockTaskStore) ListRacksWithWaitingTasks(_ context.Context) ([]uuid.UUID, error) {
	panic("mockTaskStore.ListRacksWithWaitingTasks: not implemented")
}

func (m *mockTaskStore) CreateRule(_ context.Context, _ *operationrules.OperationRule) error {
	panic("mockTaskStore.CreateRule: not implemented")
}

func (m *mockTaskStore) UpdateRule(_ context.Context, _ uuid.UUID, _ map[string]interface{}) error {
	panic("mockTaskStore.UpdateRule: not implemented")
}

func (m *mockTaskStore) DeleteRule(_ context.Context, _ uuid.UUID) error {
	panic("mockTaskStore.DeleteRule: not implemented")
}

func (m *mockTaskStore) SetRuleAsDefault(_ context.Context, _ uuid.UUID) error {
	panic("mockTaskStore.SetRuleAsDefault: not implemented")
}

func (m *mockTaskStore) GetRule(_ context.Context, _ uuid.UUID) (*operationrules.OperationRule, error) {
	panic("mockTaskStore.GetRule: not implemented")
}

func (m *mockTaskStore) GetRuleByName(_ context.Context, _ string) (*operationrules.OperationRule, error) {
	panic("mockTaskStore.GetRuleByName: not implemented")
}

func (m *mockTaskStore) GetDefaultRule(_ context.Context, _ taskcommon.TaskType, _ string) (*operationrules.OperationRule, error) {
	panic("mockTaskStore.GetDefaultRule: not implemented")
}

func (m *mockTaskStore) GetRuleByOperationAndRack(_ context.Context, _ taskcommon.TaskType, _ string, _ *uuid.UUID) (*operationrules.OperationRule, error) {
	panic("mockTaskStore.GetRuleByOperationAndRack: not implemented")
}

func (m *mockTaskStore) ListRules(_ context.Context, _ *taskcommon.OperationRuleListOptions, _ *dbquery.Pagination) ([]*operationrules.OperationRule, int32, error) {
	panic("mockTaskStore.ListRules: not implemented")
}

func (m *mockTaskStore) AssociateRuleWithRack(_ context.Context, _ uuid.UUID, _ uuid.UUID) error {
	panic("mockTaskStore.AssociateRuleWithRack: not implemented")
}

func (m *mockTaskStore) DisassociateRuleFromRack(_ context.Context, _ uuid.UUID, _ taskcommon.TaskType, _ string) error {
	panic("mockTaskStore.DisassociateRuleFromRack: not implemented")
}

func (m *mockTaskStore) GetRackRuleAssociation(_ context.Context, _ uuid.UUID, _ taskcommon.TaskType, _ string) (*uuid.UUID, error) {
	panic("mockTaskStore.GetRackRuleAssociation: not implemented")
}

func (m *mockTaskStore) ListRackRuleAssociations(_ context.Context, _ uuid.UUID) ([]*operationrules.RackRuleAssociation, error) {
	panic("mockTaskStore.ListRackRuleAssociations: not implemented")
}

// mockTaskManager implements taskmanager.Manager with a configurable SubmitTask.
type mockTaskManager struct {
	submitTaskFn func(ctx context.Context, req *operation.Request) ([]uuid.UUID, error)
}

func (m *mockTaskManager) Start(_ context.Context) error { return nil }
func (m *mockTaskManager) Stop(_ context.Context)        {}

func (m *mockTaskManager) SubmitTask(ctx context.Context, req *operation.Request) ([]uuid.UUID, error) {
	if m.submitTaskFn != nil {
		return m.submitTaskFn(ctx, req)
	}
	panic("mockTaskManager.SubmitTask: not set")
}

func (m *mockTaskManager) CancelTask(_ context.Context, _ uuid.UUID) error {
	panic("mockTaskManager.CancelTask: not implemented")
}

// Compile-time interface checks.
var _ Store = (*mockScheduleStore)(nil)
var _ taskstore.Store = (*mockTaskStore)(nil)

// ─── helpers ──────────────────────────────────────────────────────────────────

// ptrTime returns a pointer to t.
func ptrTime(t time.Time) *time.Time { return &t }

// mustMarshalFilter marshals a ComponentFilter for use in scope rows.
func mustMarshalFilter(t *testing.T, cf *dbmodel.ComponentFilter) json.RawMessage {
	t.Helper()
	raw, err := dbmodel.MarshalComponentFilter(cf)
	require.NoError(t, err)
	return raw
}

// newDispatcher builds a Dispatcher wired to the given mocks.
func newDispatcher(store Store, ts taskstore.Store, tm *mockTaskManager) *Dispatcher {
	return &Dispatcher{
		store:       store,
		taskStore:   ts,
		taskManager: tm,
	}
}

// ─── TestScopeToTargetSpec ────────────────────────────────────────────────────

func TestScopeToTargetSpec(t *testing.T) {
	rackID := uuid.MustParse("aaaaaaaa-0000-0000-0000-000000000001")
	compA := uuid.MustParse("bbbbbbbb-0000-0000-0000-000000000002")
	compB := uuid.MustParse("cccccccc-0000-0000-0000-000000000003")

	cases := map[string]struct {
		scope   *dbmodel.TaskScheduleScope
		want    operation.TargetSpec
		wantErr bool
	}{
		"nil filter — all components in rack": {
			scope: &dbmodel.TaskScheduleScope{RackID: rackID, ComponentFilter: nil},
			want: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{Identifier: identifier.Identifier{ID: rackID}},
				},
			},
		},
		"types filter — rack target with component types": {
			scope: &dbmodel.TaskScheduleScope{
				RackID: rackID,
				ComponentFilter: mustMarshalFilter(t, &dbmodel.ComponentFilter{
					Kind:  dbmodel.ComponentFilterKindTypes,
					Types: []string{"COMPUTE", "NVSWITCH"},
				}),
			},
			want: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{
						Identifier: identifier.Identifier{ID: rackID},
						ComponentTypes: []devicetypes.ComponentType{
							devicetypes.ComponentTypeCompute,
							devicetypes.ComponentTypeNVSwitch,
						},
					},
				},
			},
		},
		"types filter — mixed types across categories": {
			// COMPUTE (compute), NVSWITCH (network), POWERSHELF (power), TORSWITCH (ToR)
			// exercises the full ComponentTypeFromString mapping loop with diverse types.
			scope: &dbmodel.TaskScheduleScope{
				RackID: rackID,
				ComponentFilter: mustMarshalFilter(t, &dbmodel.ComponentFilter{
					Kind:  dbmodel.ComponentFilterKindTypes,
					Types: []string{"COMPUTE", "NVSWITCH", "POWERSHELF", "TORSWITCH"},
				}),
			},
			want: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{
						Identifier: identifier.Identifier{ID: rackID},
						ComponentTypes: []devicetypes.ComponentType{
							devicetypes.ComponentTypeCompute,
							devicetypes.ComponentTypeNVSwitch,
							devicetypes.ComponentTypePowerShelf,
							devicetypes.ComponentTypeToRSwitch,
						},
					},
				},
			},
		},
		"components filter — individual component targets": {
			scope: &dbmodel.TaskScheduleScope{
				RackID: rackID,
				ComponentFilter: mustMarshalFilter(t, &dbmodel.ComponentFilter{
					Kind:       dbmodel.ComponentFilterKindComponents,
					Components: []uuid.UUID{compA, compB},
				}),
			},
			want: operation.TargetSpec{
				Components: []operation.ComponentTarget{
					{UUID: compA},
					{UUID: compB},
				},
			},
		},
		// Mismatch cases: valid JSON that bypasses MarshalComponentFilter but fails
		// UnmarshalComponentFilter validation inside scopeToTargetSpec. This simulates
		// a corrupted or manually edited DB row.
		"kind=types but components field populated instead — error": {
			// kind claims "types" but no types array is present; components is set instead.
			scope: &dbmodel.TaskScheduleScope{
				RackID: rackID,
				ComponentFilter: json.RawMessage(
					`{"kind":"types","components":["aaaaaaaa-0000-0000-0000-000000000001"]}`,
				),
			},
			wantErr: true,
		},
		"kind=components but types field populated instead — error": {
			// kind claims "components" but no components array is present; types is set instead.
			scope: &dbmodel.TaskScheduleScope{
				RackID: rackID,
				ComponentFilter: json.RawMessage(
					`{"kind":"components","types":["COMPUTE"]}`,
				),
			},
			wantErr: true,
		},
		"invalid JSON — error": {
			scope:   &dbmodel.TaskScheduleScope{RackID: rackID, ComponentFilter: json.RawMessage(`not-json`)},
			wantErr: true,
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := scopeToTargetSpec(tc.scope)
			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

// ─── TestNextCronTime ─────────────────────────────────────────────────────────

func TestNextCronTime(t *testing.T) {
	// reference: 2026-01-15 10:30:00 UTC (Thursday)
	ref := time.Date(2026, 1, 15, 10, 30, 0, 0, time.UTC)

	cases := map[string]struct {
		spec    string
		tz      string
		want    time.Time
		wantErr bool
	}{
		"top of next hour": {
			spec: "0 * * * *",
			tz:   "UTC",
			want: time.Date(2026, 1, 15, 11, 0, 0, 0, time.UTC),
		},
		"daily at 02:00 UTC": {
			spec: "0 2 * * *",
			tz:   "UTC",
			want: time.Date(2026, 1, 16, 2, 0, 0, 0, time.UTC),
		},
		"timezone shifts the window — LA is UTC-8": {
			// "0 2 * * *" in America/Los_Angeles = 10:00 UTC
			// ref is 10:30 UTC so the next occurrence is tomorrow at 10:00 UTC.
			spec: "0 2 * * *",
			tz:   "America/Los_Angeles",
			want: time.Date(2026, 1, 16, 10, 0, 0, 0, time.UTC),
		},
		"invalid cron spec": {
			spec:    "not a cron",
			tz:      "UTC",
			wantErr: true,
		},
		"invalid timezone": {
			spec:    "0 * * * *",
			tz:      "Invalid/Zone",
			wantErr: true,
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := nextCronTime(tc.spec, tc.tz, ref)
			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

// ─── TestComputeFirstRunAt ────────────────────────────────────────────────────

func TestComputeFirstRunAt(t *testing.T) {
	cases := map[string]struct {
		specType dbmodel.SpecType
		spec     string
		tz       string
		wantErr  bool
		// checkFn validates the returned time for time-dependent cases.
		checkFn func(t *testing.T, before time.Time, got time.Time, after time.Time)
	}{
		"one-time valid RFC3339": {
			specType: dbmodel.SpecTypeOneTime,
			spec:     "2026-06-01T04:00:00Z",
			checkFn: func(t *testing.T, _, got, _ time.Time) {
				t.Helper()
				want := time.Date(2026, 6, 1, 4, 0, 0, 0, time.UTC)
				assert.Equal(t, want, got)
			},
		},
		"one-time invalid format": {
			specType: dbmodel.SpecTypeOneTime,
			spec:     "not-a-timestamp",
			wantErr:  true,
		},
		"interval valid — result is approximately now+duration": {
			specType: dbmodel.SpecTypeInterval,
			spec:     "2h",
			checkFn: func(t *testing.T, before, got, after time.Time) {
				t.Helper()
				assert.True(t, got.After(before.Add(2*time.Hour)) || got.Equal(before.Add(2*time.Hour)))
				assert.True(t, got.Before(after.Add(2*time.Hour)) || got.Equal(after.Add(2*time.Hour)))
			},
		},
		"interval invalid spec": {
			specType: dbmodel.SpecTypeInterval,
			spec:     "not-a-duration",
			wantErr:  true,
		},
		"interval zero duration": {
			specType: dbmodel.SpecTypeInterval,
			spec:     "0s",
			wantErr:  true,
		},
		"interval negative duration": {
			specType: dbmodel.SpecTypeInterval,
			spec:     "-1h",
			wantErr:  true,
		},
		"cron valid — result is after now": {
			specType: dbmodel.SpecTypeCron,
			spec:     "0 * * * *",
			tz:       "UTC",
			checkFn: func(t *testing.T, before, got, _ time.Time) {
				t.Helper()
				assert.True(t, got.After(before), "cron next time should be after now")
			},
		},
		"cron invalid spec": {
			specType: dbmodel.SpecTypeCron,
			spec:     "bad cron",
			tz:       "UTC",
			wantErr:  true,
		},
		"cron invalid timezone": {
			specType: dbmodel.SpecTypeCron,
			spec:     "0 * * * *",
			tz:       "Bad/Zone",
			wantErr:  true,
		},
		"unknown spec_type": {
			specType: dbmodel.SpecType("unknown"),
			spec:     "anything",
			wantErr:  true,
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			before := time.Now()
			got, err := ComputeFirstRunAt(tc.specType, tc.spec, tc.tz)
			after := time.Now()

			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			if tc.checkFn != nil {
				tc.checkFn(t, before, got, after)
			}
		})
	}
}

// ─── TestApplyAfterFire ───────────────────────────────────────────────────────

func TestApplyAfterFire(t *testing.T) {
	now := time.Date(2026, 1, 15, 10, 0, 0, 0, time.UTC)
	schedID := uuid.MustParse("dddddddd-0000-0000-0000-000000000001")

	cases := map[string]struct {
		ts         *dbmodel.TaskSchedule
		wantErr    string
		wantUpdate AfterFireUpdate
	}{
		"one-time — consumed, Enabled=false, NextRunAt=nil": {
			ts: &dbmodel.TaskSchedule{
				ID: schedID, SpecType: dbmodel.SpecTypeOneTime, Enabled: true,
			},
			wantUpdate: AfterFireUpdate{
				LastRunAt: now,
				Enabled:   false,
				NextRunAt: nil,
			},
		},
		"interval — NextRunAt advances by duration, enabled preserved": {
			ts: &dbmodel.TaskSchedule{
				ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "24h", Enabled: true,
			},
			wantUpdate: AfterFireUpdate{
				LastRunAt: now,
				Enabled:   true,
				NextRunAt: ptrTime(now.Add(24 * time.Hour)),
			},
		},
		"interval — enabled=false preserved (disabled schedule manually triggered)": {
			ts: &dbmodel.TaskSchedule{
				ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "1h", Enabled: false,
			},
			wantUpdate: AfterFireUpdate{
				LastRunAt: now,
				Enabled:   false,
				NextRunAt: ptrTime(now.Add(time.Hour)),
			},
		},
		"interval — invalid spec": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "not-a-duration"},
			wantErr: "invalid interval spec",
		},
		"interval — zero duration": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "0s"},
			wantErr: "must be a positive duration",
		},
		"cron — NextRunAt set to next cron time": {
			ts: &dbmodel.TaskSchedule{
				ID:       schedID,
				SpecType: dbmodel.SpecTypeCron,
				Spec:     "0 * * * *", // top of every hour
				Timezone: "UTC",
				Enabled:  true,
			},
			// now=10:00 → next cron time at 11:00
			wantUpdate: AfterFireUpdate{
				LastRunAt: now,
				Enabled:   true,
				NextRunAt: ptrTime(time.Date(2026, 1, 15, 11, 0, 0, 0, time.UTC)),
			},
		},
		"cron — invalid spec": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeCron, Spec: "bad cron", Timezone: "UTC"},
			wantErr: "advance cron spec",
		},
		"cron — invalid timezone": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeCron, Spec: "0 * * * *", Timezone: "Invalid/Zone"},
			wantErr: "advance cron spec",
		},
		"unknown spec_type": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecType("unknown")},
			wantErr: "unknown spec_type",
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var captured AfterFireUpdate
			store := &mockScheduleStore{
				updateAfterFireFn: func(_ context.Context, _ uuid.UUID, u AfterFireUpdate) error {
					captured = u
					return nil
				},
			}
			d := newDispatcher(store, nil, nil)

			err := d.applyAfterFire(context.Background(), tc.ts, now)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantUpdate.LastRunAt, captured.LastRunAt)
			assert.Equal(t, tc.wantUpdate.Enabled, captured.Enabled)
			if tc.wantUpdate.NextRunAt == nil {
				assert.Nil(t, captured.NextRunAt)
			} else {
				require.NotNil(t, captured.NextRunAt)
				assert.Equal(t, *tc.wantUpdate.NextRunAt, *captured.NextRunAt)
			}
		})
	}
}

// ─── TestAdvanceOnly ──────────────────────────────────────────────────────────

func TestAdvanceOnly(t *testing.T) {
	now := time.Date(2026, 1, 15, 10, 0, 0, 0, time.UTC)
	schedID := uuid.MustParse("eeeeeeee-0000-0000-0000-000000000001")

	cases := map[string]struct {
		ts              *dbmodel.TaskSchedule
		wantErr         string
		wantAfterFire   *AfterFireUpdate // set when UpdateAfterFire should be called
		wantUpdateField *time.Time       // expected NextRunAt passed to Update
	}{
		"one-time — calls UpdateAfterFire, consumed": {
			ts: &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeOneTime},
			wantAfterFire: &AfterFireUpdate{
				LastRunAt: now,
				Enabled:   false,
				NextRunAt: nil,
			},
		},
		"interval valid — calls Update with next run": {
			ts:              &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "6h"},
			wantUpdateField: ptrTime(now.Add(6 * time.Hour)),
		},
		"interval invalid spec — error": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "bad"},
			wantErr: "invalid interval spec",
		},
		"interval zero duration — error": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeInterval, Spec: "0h"},
			wantErr: "must be a positive duration",
		},
		"cron valid — calls Update with next run": {
			ts:              &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeCron, Spec: "0 * * * *", Timezone: "UTC"},
			wantUpdateField: ptrTime(time.Date(2026, 1, 15, 11, 0, 0, 0, time.UTC)),
		},
		"cron invalid spec — error": {
			ts:      &dbmodel.TaskSchedule{ID: schedID, SpecType: dbmodel.SpecTypeCron, Spec: "bad cron", Timezone: "UTC"},
			wantErr: "advance cron spec",
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var afterFireCapture *AfterFireUpdate
			var updateCapture *time.Time

			store := &mockScheduleStore{
				updateAfterFireFn: func(_ context.Context, _ uuid.UUID, u AfterFireUpdate) error {
					afterFireCapture = &u
					return nil
				},
				updateFn: func(_ context.Context, _ uuid.UUID, f UpdateFields) (*dbmodel.TaskSchedule, error) {
					updateCapture = f.NextRunAt
					return &dbmodel.TaskSchedule{ID: schedID}, nil
				},
			}
			d := newDispatcher(store, nil, nil)

			err := d.advanceOnly(context.Background(), tc.ts, now)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)

			if tc.wantAfterFire != nil {
				require.NotNil(t, afterFireCapture)
				assert.Equal(t, tc.wantAfterFire.LastRunAt, afterFireCapture.LastRunAt)
				assert.Equal(t, tc.wantAfterFire.Enabled, afterFireCapture.Enabled)
				assert.Nil(t, afterFireCapture.NextRunAt)
			}
			if tc.wantUpdateField != nil {
				require.NotNil(t, updateCapture)
				assert.Equal(t, *tc.wantUpdateField, *updateCapture)
			}
		})
	}
}

// ─── TestFilterScopesByPolicy ─────────────────────────────────────────────────

func TestFilterScopesByPolicy(t *testing.T) {
	schedID := uuid.MustParse("ffffffff-0000-0000-0000-000000000001")
	taskActive := uuid.MustParse("aaaaaaaa-1111-0000-0000-000000000001")
	taskDone := uuid.MustParse("bbbbbbbb-1111-0000-0000-000000000001")

	makeScope := func(id uuid.UUID, lastTaskID *uuid.UUID) *dbmodel.TaskScheduleScope {
		return &dbmodel.TaskScheduleScope{ID: id, LastTaskID: lastTaskID}
	}

	activeStatuses := []taskcommon.TaskStatus{
		taskcommon.TaskStatusWaiting,
		taskcommon.TaskStatusPending,
		taskcommon.TaskStatusRunning,
	}
	finishedStatuses := []taskcommon.TaskStatus{
		taskcommon.TaskStatusCompleted,
		taskcommon.TaskStatusFailed,
		taskcommon.TaskStatusTerminated,
	}

	t.Run("queue policy — all scopes returned unconditionally", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{ID: schedID, OverlapPolicy: dbmodel.OverlapPolicyQueue}
		s1 := makeScope(uuid.New(), &taskActive)
		s2 := makeScope(uuid.New(), nil)
		d := newDispatcher(nil, nil, nil)

		got, err := d.filterScopesByPolicy(context.Background(), ts, []*dbmodel.TaskScheduleScope{s1, s2})
		require.NoError(t, err)
		assert.Equal(t, []*dbmodel.TaskScheduleScope{s1, s2}, got)
	})

	t.Run("skip policy — nil last_task_id always included", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{ID: schedID, OverlapPolicy: dbmodel.OverlapPolicySkip}
		scope := makeScope(uuid.New(), nil)
		d := newDispatcher(nil, nil, nil)

		got, err := d.filterScopesByPolicy(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope})
		require.NoError(t, err)
		assert.Equal(t, []*dbmodel.TaskScheduleScope{scope}, got)
	})

	for _, status := range activeStatuses {
		t.Run(fmt.Sprintf("skip policy — %s task excluded", status), func(t *testing.T) {
			ts := &dbmodel.TaskSchedule{ID: schedID, OverlapPolicy: dbmodel.OverlapPolicySkip}
			scope := makeScope(uuid.New(), &taskActive)
			taskStore := &mockTaskStore{
				getTaskFn: func(_ context.Context, id uuid.UUID) (*taskdef.Task, error) {
					require.Equal(t, taskActive, id)
					return &taskdef.Task{Status: status}, nil
				},
			}
			d := newDispatcher(nil, taskStore, nil)

			got, err := d.filterScopesByPolicy(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope})
			require.NoError(t, err)
			assert.Empty(t, got)
		})
	}

	for _, status := range finishedStatuses {
		t.Run(fmt.Sprintf("skip policy — %s task included", status), func(t *testing.T) {
			ts := &dbmodel.TaskSchedule{ID: schedID, OverlapPolicy: dbmodel.OverlapPolicySkip}
			scope := makeScope(uuid.New(), &taskDone)
			taskStore := &mockTaskStore{
				getTaskFn: func(_ context.Context, _ uuid.UUID) (*taskdef.Task, error) {
					return &taskdef.Task{Status: status}, nil
				},
			}
			d := newDispatcher(nil, taskStore, nil)

			got, err := d.filterScopesByPolicy(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope})
			require.NoError(t, err)
			assert.Equal(t, []*dbmodel.TaskScheduleScope{scope}, got)
		})
	}

	t.Run("skip policy — GetTask error propagated", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{ID: schedID, OverlapPolicy: dbmodel.OverlapPolicySkip}
		scope := makeScope(uuid.New(), &taskActive)
		taskStore := &mockTaskStore{
			getTaskFn: func(_ context.Context, _ uuid.UUID) (*taskdef.Task, error) {
				return nil, errors.New("db down")
			},
		}
		d := newDispatcher(nil, taskStore, nil)

		_, err := d.filterScopesByPolicy(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope})
		require.ErrorContains(t, err, "db down")
	})

	t.Run("skip policy — partial: one active, one done", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{ID: schedID, OverlapPolicy: dbmodel.OverlapPolicySkip}
		scopeActive := makeScope(uuid.New(), &taskActive)
		scopeDone := makeScope(uuid.New(), &taskDone)
		taskStore := &mockTaskStore{
			getTaskFn: func(_ context.Context, id uuid.UUID) (*taskdef.Task, error) {
				if id == taskActive {
					return &taskdef.Task{Status: taskcommon.TaskStatusRunning}, nil
				}
				return &taskdef.Task{Status: taskcommon.TaskStatusCompleted}, nil
			},
		}
		d := newDispatcher(nil, taskStore, nil)

		got, err := d.filterScopesByPolicy(context.Background(), ts, []*dbmodel.TaskScheduleScope{scopeActive, scopeDone})
		require.NoError(t, err)
		assert.Equal(t, []*dbmodel.TaskScheduleScope{scopeDone}, got)
	})
}

// ─── TestSubmitScopeTasks ─────────────────────────────────────────────────────

func TestSubmitScopeTasks(t *testing.T) {
	now := time.Date(2026, 1, 15, 10, 0, 0, 0, time.UTC)
	rackID := uuid.MustParse("11111111-0000-0000-0000-000000000001")
	scopeID := uuid.MustParse("22222222-0000-0000-0000-000000000001")
	taskID := uuid.MustParse("33333333-0000-0000-0000-000000000001")

	validTemplate := mustMarshalTemplate(
		taskcommon.TaskTypePowerControl,
		taskcommon.OpCodePowerControlPowerOn,
		json.RawMessage(`{}`),
	)

	t.Run("invalid operation template — error before any SubmitTask", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{OperationTemplate: json.RawMessage(`not-json`)}
		d := newDispatcher(nil, nil, nil)

		_, _, err := d.submitScopeTasks(context.Background(), ts, []*dbmodel.TaskScheduleScope{{ID: scopeID, RackID: rackID}}, now)
		require.Error(t, err)
	})

	t.Run("nil component filter — rack target covers all components", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{
			ID: uuid.New(), Name: "sched", OperationTemplate: validTemplate,
		}
		scope := &dbmodel.TaskScheduleScope{ID: scopeID, RackID: rackID, ComponentFilter: nil}

		var capturedReq *operation.Request
		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, req *operation.Request) ([]uuid.UUID, error) {
				capturedReq = req
				return []uuid.UUID{taskID}, nil
			},
		}
		d := newDispatcher(nil, nil, mgr)

		ids, taskIDs, err := d.submitScopeTasks(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope}, now)
		require.NoError(t, err)
		require.Equal(t, map[uuid.UUID]uuid.UUID{scopeID: taskID}, ids)
		require.Equal(t, []uuid.UUID{taskID}, taskIDs)
		require.NotNil(t, capturedReq)
		assert.Equal(t, rackID, capturedReq.RequiredRackID)
		require.Len(t, capturedReq.TargetSpec.Racks, 1)
		assert.Equal(t, rackID, capturedReq.TargetSpec.Racks[0].Identifier.ID)
		assert.Empty(t, capturedReq.TargetSpec.Racks[0].ComponentTypes)
	})

	t.Run("conflict strategy and rule ID flow through to submitted request", func(t *testing.T) {
		ruleUUID := uuid.MustParse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
		tmpl, err := MarshalTemplate(
			taskcommon.TaskTypePowerControl,
			taskcommon.OpCodePowerControlPowerOn,
			json.RawMessage(`{}`),
			TemplateOptions{
				QueueTimeoutSecs: 30,
				RuleID:           ruleUUID.String(),
			},
		)
		require.NoError(t, err)

		// ConflictStrategy is derived from OverlapPolicy at fire time, not from the template.
		ts := &dbmodel.TaskSchedule{
			ID:                uuid.New(),
			Name:              "sched",
			OperationTemplate: tmpl,
			OverlapPolicy:     dbmodel.OverlapPolicyQueue,
		}
		scope := &dbmodel.TaskScheduleScope{ID: scopeID, RackID: rackID}

		var capturedReq *operation.Request
		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, req *operation.Request) ([]uuid.UUID, error) {
				capturedReq = req
				return []uuid.UUID{taskID}, nil
			},
		}
		d := newDispatcher(nil, nil, mgr)

		_, _, err = d.submitScopeTasks(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope}, now)
		require.NoError(t, err)
		require.NotNil(t, capturedReq)
		assert.Equal(t, operation.ConflictStrategyQueue, capturedReq.ConflictStrategy)
		assert.Equal(t, 30*time.Second, capturedReq.QueueTimeout)
		require.NotNil(t, capturedReq.RuleID)
		assert.Equal(t, ruleUUID, *capturedReq.RuleID)
	})

	t.Run("SubmitTask error — scope skipped, all-fail error returned", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{ID: uuid.New(), Name: "sched", OperationTemplate: validTemplate}
		scope := &dbmodel.TaskScheduleScope{ID: scopeID, RackID: rackID}

		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, _ *operation.Request) ([]uuid.UUID, error) {
				return nil, errors.New("task manager error")
			},
		}
		d := newDispatcher(nil, nil, mgr)

		ids, taskIDs, err := d.submitScopeTasks(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope}, now)
		require.ErrorContains(t, err, "all")
		assert.Nil(t, ids)
		assert.Nil(t, taskIDs)
	})

	t.Run("SubmitTask returns empty IDs — scope skipped", func(t *testing.T) {
		ts := &dbmodel.TaskSchedule{ID: uuid.New(), Name: "sched", OperationTemplate: validTemplate}
		scope := &dbmodel.TaskScheduleScope{ID: scopeID, RackID: rackID}

		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, _ *operation.Request) ([]uuid.UUID, error) {
				return []uuid.UUID{}, nil // empty, no error
			},
		}
		d := newDispatcher(nil, nil, mgr)

		ids, taskIDs, err := d.submitScopeTasks(context.Background(), ts, []*dbmodel.TaskScheduleScope{scope}, now)
		require.Error(t, err)
		assert.Nil(t, ids)
		assert.Nil(t, taskIDs)
	})

	t.Run("partial success — failing scope skipped, succeeding scope recorded", func(t *testing.T) {
		scopeOK := &dbmodel.TaskScheduleScope{ID: uuid.New(), RackID: rackID}
		scopeFail := &dbmodel.TaskScheduleScope{ID: uuid.New(), RackID: rackID}
		ts := &dbmodel.TaskSchedule{ID: uuid.New(), Name: "sched", OperationTemplate: validTemplate}

		// Use a submission counter to distinguish the two scopes (both share the same rackID).
		callCount := 0
		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, _ *operation.Request) ([]uuid.UUID, error) {
				callCount++
				if callCount == 1 {
					return []uuid.UUID{taskID}, nil // scopeOK succeeds
				}
				return nil, errors.New("failed") // scopeFail fails
			},
		}

		d := newDispatcher(nil, nil, mgr)
		ids, taskIDs, err := d.submitScopeTasks(context.Background(), ts, []*dbmodel.TaskScheduleScope{scopeOK, scopeFail}, now)
		require.NoError(t, err)
		assert.Equal(t, map[uuid.UUID]uuid.UUID{scopeOK.ID: taskID}, ids)
		assert.Equal(t, []uuid.UUID{taskID}, taskIDs)
	})
}

// ─── TestFireNow ──────────────────────────────────────────────────────────────

func TestFireNow(t *testing.T) {
	schedID := uuid.MustParse("44444444-0000-0000-0000-000000000001")
	rackID := uuid.MustParse("55555555-0000-0000-0000-000000000001")
	scopeID := uuid.MustParse("66666666-0000-0000-0000-000000000001")
	taskID := uuid.MustParse("77777777-0000-0000-0000-000000000001")

	validTemplate := mustMarshalTemplate(
		taskcommon.TaskTypePowerControl,
		taskcommon.OpCodePowerControlPowerOn,
		json.RawMessage(`{}`),
	)

	t.Run("one-time already consumed — error", func(t *testing.T) {
		store := &mockScheduleStore{
			lockForTriggerFn: func(_ context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error) {
				return &dbmodel.TaskSchedule{
					ID:       schedID,
					SpecType: dbmodel.SpecTypeOneTime,
					Enabled:  false, // consumed
				}, nil
			},
		}
		d := newDispatcher(store, nil, nil)

		_, err := d.FireNow(context.Background(), schedID)
		require.ErrorContains(t, err, "one-time schedule that has already fired")
	})

	t.Run("no scopes — advances state, returns nil task IDs", func(t *testing.T) {
		store := &mockScheduleStore{
			lockForTriggerFn: func(_ context.Context, _ uuid.UUID) (*dbmodel.TaskSchedule, error) {
				return &dbmodel.TaskSchedule{
					ID:       schedID,
					SpecType: dbmodel.SpecTypeOneTime,
					Enabled:  true,
				}, nil
			},
			listScopesFn: func(_ context.Context, _ uuid.UUID) ([]*dbmodel.TaskScheduleScope, error) {
				return nil, nil
			},
		}
		d := newDispatcher(store, nil, nil)

		taskIDs, err := d.FireNow(context.Background(), schedID)
		require.NoError(t, err)
		assert.Nil(t, taskIDs)
	})

	t.Run("happy path — interval, task IDs returned, scope updated", func(t *testing.T) {
		var scopeLastTaskIDs map[uuid.UUID]uuid.UUID
		store := &mockScheduleStore{
			lockForTriggerFn: func(_ context.Context, _ uuid.UUID) (*dbmodel.TaskSchedule, error) {
				return &dbmodel.TaskSchedule{
					ID:                schedID,
					Name:              "test-schedule",
					SpecType:          dbmodel.SpecTypeInterval,
					Spec:              "1h",
					Enabled:           true,
					OperationTemplate: validTemplate,
				}, nil
			},
			listScopesFn: func(_ context.Context, _ uuid.UUID) ([]*dbmodel.TaskScheduleScope, error) {
				return []*dbmodel.TaskScheduleScope{
					{ID: scopeID, RackID: rackID},
				}, nil
			},
			updateScopeLastTaskIDsFn: func(_ context.Context, m map[uuid.UUID]uuid.UUID) error {
				scopeLastTaskIDs = m
				return nil
			},
		}
		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, _ *operation.Request) ([]uuid.UUID, error) {
				return []uuid.UUID{taskID}, nil
			},
		}
		d := newDispatcher(store, nil, mgr)

		taskIDs, err := d.FireNow(context.Background(), schedID)
		require.NoError(t, err)
		assert.Equal(t, []uuid.UUID{taskID}, taskIDs)
		assert.Equal(t, map[uuid.UUID]uuid.UUID{scopeID: taskID}, scopeLastTaskIDs)
	})

	t.Run("all scope submissions fail — error returned", func(t *testing.T) {
		store := &mockScheduleStore{
			lockForTriggerFn: func(_ context.Context, _ uuid.UUID) (*dbmodel.TaskSchedule, error) {
				return &dbmodel.TaskSchedule{
					ID:                schedID,
					Name:              "test-schedule",
					SpecType:          dbmodel.SpecTypeInterval,
					Spec:              "1h",
					Enabled:           true,
					OperationTemplate: validTemplate,
				}, nil
			},
			listScopesFn: func(_ context.Context, _ uuid.UUID) ([]*dbmodel.TaskScheduleScope, error) {
				return []*dbmodel.TaskScheduleScope{
					{ID: scopeID, RackID: rackID},
				}, nil
			},
		}
		mgr := &mockTaskManager{
			submitTaskFn: func(_ context.Context, _ *operation.Request) ([]uuid.UUID, error) {
				return nil, errors.New("submit failed")
			},
		}
		d := newDispatcher(store, nil, mgr)

		_, err := d.FireNow(context.Background(), schedID)
		require.Error(t, err)
	})

	t.Run("LockForTrigger error — propagated", func(t *testing.T) {
		store := &mockScheduleStore{
			lockForTriggerFn: func(_ context.Context, _ uuid.UUID) (*dbmodel.TaskSchedule, error) {
				return nil, errors.New("db error")
			},
		}
		d := newDispatcher(store, nil, nil)

		_, err := d.FireNow(context.Background(), schedID)
		require.ErrorContains(t, err, "db error")
	})
}
