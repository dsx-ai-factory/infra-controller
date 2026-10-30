// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
)

type fixedTimestampSource struct {
	now *time.Time
}

type pauseAfterSharedBindingCheck struct {
	once        sync.Once
	releaseOnce sync.Once
	reached     chan struct{}
	release     chan struct{}
}

func (h *pauseAfterSharedBindingCheck) BeforeQuery(
	ctx context.Context,
	_ *bun.QueryEvent,
) context.Context {
	return ctx
}

func (h *pauseAfterSharedBindingCheck) AfterQuery(
	_ context.Context,
	event *bun.QueryEvent,
) {
	if !strings.Contains(event.Query, "erb.id <>") {
		return
	}

	h.once.Do(func() {
		close(h.reached)
		<-h.release
	})
}

func (h *pauseAfterSharedBindingCheck) resume() {
	h.releaseOnce.Do(func() {
		close(h.release)
	})
}

func (s fixedTimestampSource) Timestamp(
	_ context.Context,
	_ bun.IDB,
) (time.Time, error) {
	return s.now.UTC(), nil
}

func TestStore_RuleBindingContract(t *testing.T) {
	session := storetest.NewPostgresTestSession(t)

	storetest.RunRuleBindingContract(t, func() (eventrule.RuleStore, eventrule.BindingStore) {
		resetEventRuleTables(session)
		store := New(session)

		return store, store
	})
}

func TestStore_ExecutionContract(t *testing.T) {
	session := storetest.NewPostgresTestSession(t)

	storetest.RunExecutionContract(t, func(now *time.Time) storetest.EventExecutionStore {
		resetEventRuleTables(session)
		store := New(session)
		store.timestamps = fixedTimestampSource{now: now}

		return store
	})
}

func TestStore_ListRackOverrides(t *testing.T) {
	store := newTestStore(t)
	ctx := t.Context()
	rule := leakage.DefaultRule().Clone()
	rule.ID, rule.Origin = uuid.New(), eventrule.RuleOriginPersisted
	shared, err := store.Create(ctx, &rule)
	require.NoError(t, err)
	// A shared rule produces two bindings; the public ordering deliberately
	// differs from canonical UUID ordering.
	ids := []uuid.UUID{uuid.MustParse("10000000-0000-4000-8000-000000000001"), uuid.MustParse("20000000-0000-4000-8000-000000000002")}
	for i, name := range []string{"rack-z", "rack-a"} {
		_, err = store.pg.DB.ExecContext(ctx, "INSERT INTO rack (id, external_id, name) VALUES (?, ?, ?)", ids[i], name, name)
		require.NoError(t, err)
		require.NoError(t, store.Bind(ctx, eventrule.Binding{ID: uuid.New(), RuleID: shared.ID, EventType: rule.EventType, Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: ids[i]}}))
	}
	// Bindings remain visible even if inventory cannot supply a public Rack ID.
	require.NoError(t, store.Bind(ctx, eventrule.Binding{ID: uuid.New(), RuleID: shared.ID, EventType: rule.EventType, Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}}))
	rule.ID = uuid.New()
	rule.Enabled = false
	disabled, err := store.Create(ctx, &rule)
	require.NoError(t, err)
	require.NoError(t, store.Bind(ctx, eventrule.Binding{ID: uuid.New(), RuleID: disabled.ID, EventType: rule.EventType, Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}}))
	rule.ID, rule.Enabled = uuid.New(), true
	_, err = store.Create(ctx, &rule) // Enabled but unbound must not count.
	require.NoError(t, err)
	for _, test := range []struct {
		name          string
		offset, limit int
		want          []string
		corrupt       bool
	}{
		{name: "shared policy ordered before slicing", limit: 10, want: []string{"rack-a", "rack-z", ""}},
		{name: "next page", offset: 1, limit: 1, want: []string{"rack-z"}},
		{name: "out of range retains binding total", offset: 50, limit: 1, want: []string{}},
		{name: "count only does not decode unreturned policies", limit: 0, corrupt: true, want: []string{}},
	} {
		t.Run(test.name, func(t *testing.T) {
			if test.corrupt {
				_, err = store.pg.DB.ExecContext(ctx, "UPDATE event_rules SET policy = '{}' WHERE id = ?", shared.ID)
				require.NoError(t, err)
			}
			rows, total, err := store.ListRackOverrides(ctx, leakage.TypeHardwareLeakDetected, test.offset, test.limit)
			require.NoError(t, err)
			require.Equal(t, 3, total)
			names := make([]string, 0, len(rows))
			for _, row := range rows {
				require.Equal(t, shared.ID, row.Rule.ID)
				require.Equal(t, eventrule.ScopeTypeRack, row.Scope.Type)
				name := ""
				if row.RackID != nil {
					name = *row.RackID
				}
				names = append(names, name)
			}
			require.Equal(t, test.want, names)
		})
	}
}

func TestStore_DeleteRuleOverride(t *testing.T) {
	ctx := t.Context()
	store := newTestStore(t)
	rule, err := store.Create(ctx, &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    eventrule.RuleOriginPersisted,
		Name:      "shared binding race",
		Enabled:   true,
		EventType: "test.event",
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	})
	require.NoError(t, err)

	firstScope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	require.NoError(t, store.Bind(ctx, eventrule.Binding{
		ID:        uuid.New(),
		RuleID:    rule.ID,
		EventType: rule.EventType,
		Scope:     firstScope,
	}))

	pause := &pauseAfterSharedBindingCheck{
		reached: make(chan struct{}),
		release: make(chan struct{}),
	}
	defer pause.resume()
	store.pg.DB.AddQueryHook(pause)

	deleteResult := make(chan error, 1)
	go func() {
		deleteResult <- store.DeleteRuleOverride(ctx, rule.EventType, firstScope)
	}()

	select {
	case <-pause.reached:
	case <-time.After(time.Second):
		require.FailNow(t, "DeleteRuleOverride did not reach the shared-binding check")
	}

	bindResult := make(chan error, 1)
	go func() {
		bindResult <- store.Bind(ctx, eventrule.Binding{
			ID:        uuid.New(),
			RuleID:    rule.ID,
			EventType: rule.EventType,
			Scope: eventrule.Scope{
				Type: eventrule.ScopeTypeRack,
				ID:   uuid.New(),
			},
		})
	}()

	var earlyBindErr error
	bindCompletedEarly := false
	select {
	case earlyBindErr = <-bindResult:
		bindCompletedEarly = true
	case <-time.After(100 * time.Millisecond):
	}
	pause.resume()

	require.NoError(t, <-deleteResult)
	if bindCompletedEarly {
		require.Failf(
			t,
			"Bind completed before DeleteRuleOverride committed",
			"Bind returned %v while deletion was paused after its ownership check",
			earlyBindErr,
		)
	}
	require.ErrorIs(t, <-bindResult, eventrule.ErrRuleNotFound)
}

func TestStore_DuplicateDoesNotResumeExecution(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	definition := testEventDefinition()
	plan := testEventPlan()

	created, err := store.CommitEventPlan(ctx, definition, plan)
	require.NoError(t, err)
	require.NotNil(t, created)

	claims, err := testPendingClaims(ctx, store, testClaimRequest("scheduler-1"))
	require.NoError(t, err)
	require.Len(t, claims, 1)

	duplicate, err := store.CommitEventPlan(ctx, definition, plan)
	require.NoError(t, err)
	require.Nil(t, duplicate)

	more, err := testPendingClaims(ctx, store, testClaimRequest("scheduler-2"))
	require.NoError(t, err)
	require.Empty(t, more)

	events, err := store.Events(ctx)
	require.NoError(t, err)
	require.Len(t, events, 1)
	require.Equal(t, 2, events[0].Observations)

	executions, err := store.Executions(ctx)
	require.NoError(t, err)
	require.Len(t, executions, 1)
	require.Equal(t, eventrule.ExecutionStatusRunning, executions[0].Status)
	require.Equal(t, claims[0].Token, executions[0].ClaimToken)
	require.Equal(t, 1, executions[0].Attempts)
}

func TestStore_CommitEventPlanRollsBack(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	_, err := store.pg.DB.ExecContext(
		ctx,
		`ALTER TABLE event_action_executions
		 ADD CONSTRAINT event_action_executions_test_rollback
		 CHECK (action_name <> 'archive')`,
	)
	require.NoError(t, err)
	t.Cleanup(func() {
		_, err := store.pg.DB.ExecContext(
			context.Background(),
			`ALTER TABLE event_action_executions
			 DROP CONSTRAINT event_action_executions_test_rollback`,
		)
		require.NoError(t, err)
	})

	definition := testEventDefinition()
	definition.EffectivePolicy.Actions = append(
		definition.EffectivePolicy.Actions,
		eventrule.Action{Name: "archive", Spec: &eventrule.Noop{Reason: "test"}},
	)
	plan := append(
		testEventPlan(),
		eventrule.PlannedExecution{
			ActionName:    "archive",
			ExecutionPlan: &eventrule.NoopPlan{Reason: "test"},
		},
	)

	created, err := store.CommitEventPlan(ctx, definition, plan)
	require.Error(t, err)
	require.Nil(t, created)

	events, err := store.Events(ctx)
	require.NoError(t, err)
	require.Empty(t, events)

	executions, err := store.Executions(ctx)
	require.NoError(t, err)
	require.Empty(t, executions)
}

func TestStore_UsesDatabaseTimeForRetryEligibility(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	_, err := store.CommitEventPlan(ctx, testEventDefinition(), testEventPlan())
	require.NoError(t, err)

	claims, err := testPendingClaims(ctx, store, testClaimRequest("scheduler-1"))
	require.NoError(t, err)
	require.Len(t, claims, 1)

	require.NoError(t, store.TransitionClaimedExecution(
		ctx,
		claims[0].Execution.ID,
		claims[0].Token,
		eventrule.DeferredExecutionResult(
			eventrule.ExecutionReasonAttemptFailed,
			"retry later",
			100*time.Millisecond,
		),
	))

	notDue, err := testRetryClaims(ctx, store, testClaimRequest("scheduler-2"))
	require.NoError(t, err)
	require.Empty(t, notDue)

	var due []eventrule.ClaimedExecution
	require.Eventually(t, func() bool {
		due, err = testRetryClaims(ctx, store, testClaimRequest("scheduler-2"))

		return err == nil && len(due) == 1
	}, time.Second, 20*time.Millisecond)
	require.NoError(t, err)
	require.Len(t, due, 1)
}

func testClaimRequest(owner string) eventrule.ExecutionClaimRequest {
	return eventrule.ExecutionClaimRequest{
		Owner:         owner,
		Limit:         1,
		ClaimDuration: time.Minute,
		MaxAttempts:   4,
	}
}

func testPendingClaims(
	ctx context.Context,
	store *Store,
	request eventrule.ExecutionClaimRequest,
) ([]eventrule.ClaimedExecution, error) {
	batch, err := store.ClaimPendingExecutions(ctx, request)

	return batch.Claims, err
}

func testRetryClaims(
	ctx context.Context,
	store *Store,
	request eventrule.ExecutionClaimRequest,
) ([]eventrule.ClaimedExecution, error) {
	batch, err := store.ClaimRetryExecutions(ctx, request)

	return batch.Claims, err
}

func newTestStore(t *testing.T) *Store {
	t.Helper()

	return New(storetest.NewPostgresTestSession(t))
}

func resetEventRuleTables(session *cdb.Session) {
	_, err := session.DB.Exec(
		"TRUNCATE TABLE event_rules, events RESTART IDENTITY CASCADE",
	)
	if err != nil {
		panic(err)
	}
}

func testEventDefinition() eventrule.Event {
	return eventrule.Event{
		Key: eventrule.EventKey{
			SourceName: "postgres_test",
			SourceKey:  uuid.NewString(),
		},
		Type:          "test.event",
		Resource:      eventrule.ResourceIdentity{Kind: eventrule.ResourceKindRack, ID: uuid.New()},
		AppliedRuleID: uuid.New(),
		EffectivePolicy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "notify", Spec: &eventrule.Noop{Reason: "test"}},
		}},
		Summary: "PostgreSQL store test",
	}
}

func testEventPlan() []eventrule.PlannedExecution {
	return []eventrule.PlannedExecution{
		{ActionName: "notify", ExecutionPlan: &eventrule.NoopPlan{Reason: "test"}},
	}
}
