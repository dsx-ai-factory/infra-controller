// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
)

type fixedTimestampSource struct {
	now *time.Time
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
