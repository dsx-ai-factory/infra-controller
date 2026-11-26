// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
)

type ruleListCountHook struct {
	afterCount func()
	fired      bool
}

func (*ruleListCountHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}

func (h *ruleListCountHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if h.fired || event.Err != nil || !strings.HasPrefix(event.Query, "SELECT count(*)") ||
		!strings.Contains(event.Query, `"event_rules"`) {
		return
	}
	h.fired = true
	h.afterCount()
}

func TestStore_List(t *testing.T) {
	firstID := uuid.MustParse("00000000-0000-0000-0000-000000000001")
	secondID := uuid.MustParse("00000000-0000-0000-0000-000000000002")
	tests := []struct {
		name        string
		offset      int
		limit       int
		wantIDs     []uuid.UUID
		wantNextIDs []uuid.UUID
	}{
		{
			name:        "empty page keeps the count snapshot",
			offset:      1,
			limit:       1,
			wantNextIDs: []uuid.UUID{secondID},
		},
		{
			name:        "populated page keeps the count snapshot",
			limit:       2,
			wantIDs:     []uuid.UUID{firstID},
			wantNextIDs: []uuid.UUID{firstID, secondID},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ctx := t.Context()
			store := newTestStore(t)
			for _, id := range []uuid.UUID{firstID, secondID} {
				_, err := store.Create(ctx, &eventrule.Rule{
					ID:        id,
					Origin:    eventrule.RuleOriginPersisted,
					Name:      "snapshot policy",
					EventType: "test.event",
					Policy: eventrule.Policy{Actions: []eventrule.Action{
						{Name: "noop", Spec: &eventrule.Noop{}},
					}},
				})
				require.NoError(t, err)
			}
			scope := eventrule.Scope{Type: eventrule.ScopeTypeSite}
			err := store.Bind(ctx, eventrule.Binding{
				ID:        uuid.New(),
				RuleID:    secondID,
				EventType: "test.event",
				Scope:     scope,
			})
			require.NoError(t, err)

			hook := &ruleListCountHook{afterCount: func() {
				// Commit on a separate connection after the count and before the
				// page scan, while the listing transaction remains open.
				err := store.Unbind(ctx, "test.event", scope, secondID)
				require.NoError(t, err)
			}}
			store.pg.DB.AddQueryHook(hook)
			bound := false
			request := eventrule.RuleListRequest{
				Filter: eventrule.RuleFilter{Bound: &bound},
				Offset: tc.offset,
				Limit:  tc.limit,
			}
			page, err := store.List(ctx, request)
			require.NoError(t, err)
			require.True(t, hook.fired, "binding removal must commit between the count and page queries")
			require.Equal(t, 1, page.Total)
			require.Len(t, page.Rules, len(tc.wantIDs))
			for i, id := range tc.wantIDs {
				require.Equal(t, id, page.Rules[i].ID)
			}

			next, err := store.List(ctx, request)
			require.NoError(t, err)
			require.Equal(t, 2, next.Total)
			require.Len(t, next.Rules, len(tc.wantNextIDs))
			for i, id := range tc.wantNextIDs {
				require.Equal(t, id, next.Rules[i].ID)
			}
		})
	}
}

func TestStore_Create(t *testing.T) {
	ctx := t.Context()
	store := newTestStore(t)
	// Retain the pgx connection and its prepared statements between writes.
	store.pg.DB.SetMaxOpenConns(1)
	store.pg.DB.SetMaxIdleConns(1)
	callerTime := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	rule := &eventrule.Rule{
		ID:          uuid.New(),
		Origin:      eventrule.RuleOriginPersisted,
		Name:        "projection rule",
		Description: "keep every returned rule field",
		Enabled:     true,
		EventType:   "test.event",
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "notify", Spec: &eventrule.Noop{Reason: "audit"}},
		}},
		CreatedAt: callerTime,
		UpdatedAt: callerTime,
	}
	var backendPID int
	err := store.pg.DB.QueryRowContext(ctx, "SELECT pg_backend_pid()").Scan(&backendPID)
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
				_, err = store.pg.DB.ExecContext(ctx, "ALTER TABLE event_rules ADD COLUMN test_added_column text")
				require.NoError(t, err)
			}
			created, err := store.Create(ctx, rule)
			require.NoError(t, err)
			require.NotZero(t, created.CreatedAt)
			require.NotEqual(t, callerTime, created.CreatedAt)
			require.Equal(t, created.CreatedAt, created.UpdatedAt)
			expected := rule.Clone()
			expected.CreatedAt = created.CreatedAt
			expected.UpdatedAt = created.UpdatedAt
			require.Equal(t, &expected, created)
			stored, err := store.GetByID(ctx, rule.ID)
			require.NoError(t, err)
			require.Equal(t, created, stored)

			var currentPID int
			err = store.pg.DB.QueryRowContext(ctx, "SELECT pg_backend_pid()").Scan(&currentPID)
			require.NoError(t, err)
			require.Equal(t, backendPID, currentPID)
			var currentPreparedName string
			err = store.pg.DB.QueryRowContext(ctx, `SELECT name FROM pg_prepared_statements
				WHERE statement LIKE 'INSERT INTO "event_rules" %'
				AND statement NOT LIKE '%pg_prepared_statements%'`).Scan(&currentPreparedName)
			require.NoError(t, err)
			if phase.addColumn {
				require.Equal(t, preparedName, currentPreparedName)
			} else {
				preparedName = currentPreparedName
			}
			// Reuse the same ID and values so the next Create uses the cached SQL.
			require.NoError(t, store.Delete(ctx, rule.ID))
		})
	}
}
