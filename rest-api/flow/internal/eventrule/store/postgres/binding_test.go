// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"database/sql"
	"strings"
	"testing"
	"time"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	inventorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/store"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
)

type bindingListSnapshotContextKey struct{}

type bindingListCountHook struct {
	afterCount func() error
	countDone  chan struct{}
	err        error
}

func (h *bindingListCountHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if ctx.Value(bindingListSnapshotContextKey{}) != nil &&
		strings.Contains(event.Query, `"event_rule_bindings"`) &&
		!strings.HasPrefix(event.Query, "SELECT count(*)") {
		// Bun can dispatch ScanAndCount's queries concurrently. Hold the page
		// query until the count and the independent writer have finished.
		select {
		case <-h.countDone:
		case <-ctx.Done():
		}
	}
	return ctx
}

func (h *bindingListCountHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if ctx.Value(bindingListSnapshotContextKey{}) == nil ||
		!strings.Contains(event.Query, `"event_rule_bindings"`) ||
		!strings.HasPrefix(event.Query, "SELECT count(*)") {
		return
	}
	defer close(h.countDone)
	h.err = event.Err
	if h.err == nil {
		h.err = h.afterCount()
	}
}

func TestStore_ListBindings(t *testing.T) {
	t.Run("first matching binding commits between count and page", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
		defer cancel()
		store := newTestStore(t)
		rule, err := store.Create(ctx, &eventrule.Rule{
			ID:        uuid.New(),
			Origin:    eventrule.RuleOriginPersisted,
			Name:      "binding snapshot policy",
			EventType: "test.event",
			Policy: eventrule.Policy{Actions: []eventrule.Action{
				{Name: "noop", Spec: &eventrule.Noop{}},
			}},
		})
		require.NoError(t, err)
		binding := eventrule.Binding{
			ID:        uuid.New(),
			RuleID:    rule.ID,
			EventType: rule.EventType,
			Scope:     eventrule.Scope{Type: eventrule.ScopeTypeSite},
		}
		hook := &bindingListCountHook{
			countDone: make(chan struct{}),
			afterCount: func() error {
				// Use an untagged context so the writer's queries bypass the
				// page-query gate and commit on a separate connection.
				_, err := store.PutBinding(ctx, binding)
				return err
			},
		}
		store.pg.DB.AddQueryHook(hook)
		request := eventrule.BindingListRequest{RuleID: rule.ID, Limit: 1}
		listCtx := context.WithValue(ctx, bindingListSnapshotContextKey{}, true)
		rows, total, err := store.ListBindings(listCtx, request)
		require.NoError(t, err)
		select {
		case <-hook.countDone:
		default:
			t.Fatal("binding creation must commit between the count and page queries")
		}
		require.NoError(t, hook.err)
		require.Zero(t, total)
		require.Empty(t, rows)

		next, total, err := store.ListBindings(ctx, request)
		require.NoError(t, err)
		require.Equal(t, 1, total)
		require.Equal(t, []eventrule.Binding{binding}, next)
	})
}

type bindingWriter func(context.Context, *Store, eventrule.Binding) error

func TestStore_Bind(t *testing.T) {
	testBindingRackRemoval(t, func(ctx context.Context, store *Store, binding eventrule.Binding) error {
		return store.Bind(ctx, binding)
	})
}

func TestStore_PutBinding(t *testing.T) {
	testBindingRackRemoval(t, func(ctx context.Context, store *Store, binding eventrule.Binding) error {
		_, err := store.PutBinding(ctx, binding)
		return err
	})
}

func testBindingRackRemoval(t *testing.T, write bindingWriter) {
	t.Helper()
	for _, tc := range []struct {
		name    string
		missing bool
		deleted bool
	}{
		{name: "missing Rack", missing: true},
		{name: "soft-deleted Rack", deleted: true},
		{name: "binding serializes with Rack removal"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx := t.Context()
			store := newTestStore(t)
			rack := &dbmodel.Rack{ID: uuid.New(), Name: "binding rack"}
			if !tc.missing {
				require.NoError(t, rack.Create(ctx, store.pg.DB))
			}
			rule, err := store.Create(ctx, &eventrule.Rule{
				ID: uuid.New(), Origin: eventrule.RuleOriginPersisted, Name: "shared policy", EventType: "test.event",
				Policy: eventrule.Policy{Actions: []eventrule.Action{{Name: "record", Spec: &eventrule.Noop{}}}},
			})
			require.NoError(t, err)
			binding := eventrule.Binding{ID: uuid.New(), RuleID: rule.ID, EventType: rule.EventType, Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rack.ID}}
			if tc.deleted {
				require.NoError(t, inventorystore.NewPostgres(store.pg).DeleteRack(ctx, rack.ID))
			}
			if tc.missing || tc.deleted {
				require.ErrorIs(t, write(ctx, store, binding), eventrule.ErrRuleTargetNotFound)
				return
			}
			testBindingDuringRackRemoval(t, store, binding, write)
		})
	}
}

func testBindingDuringRackRemoval(t *testing.T, store *Store, binding eventrule.Binding, write bindingWriter) {
	t.Helper()
	ctx := t.Context()
	gate, err := store.pg.DB.BeginTx(ctx, nil)
	require.NoError(t, err)
	var written, removed chan struct{}
	defer func() {
		rollbackErr := gate.Rollback()
		if written != nil {
			<-written
		}
		if removed != nil {
			<-removed
		}
		if rollbackErr != nil {
			require.ErrorIs(t, rollbackErr, sql.ErrTxDone)
		}
	}()
	// Hold the policy row so the real writer pauses after locking the Rack.
	var id uuid.UUID
	err = gate.NewSelect().Model((*dbmodel.EventRule)(nil)).Column("id").
		Where("er.id = ?", binding.RuleID).For("UPDATE").Scan(ctx, &id)
	require.NoError(t, err)
	var writeErr, removeErr error
	written = make(chan struct{})
	go func() {
		defer close(written)
		writeErr = write(ctx, store, binding)
	}()
	waitForBlockedBindingQuery(t, store, `%FROM "event_rules"%FOR UPDATE%`, binding.RuleID)
	removed = make(chan struct{})
	go func() {
		defer close(removed)
		removeErr = inventorystore.NewPostgres(store.pg).DeleteRack(ctx, binding.Scope.ID)
	}()
	// Removal must wait for the writer's Rack lock, then clean up its binding.
	waitForBlockedBindingQuery(t, store, `%UPDATE "rack"%`, binding.Scope.ID)
	require.NoError(t, gate.Commit())
	<-written
	<-removed
	require.NoError(t, writeErr)
	require.NoError(t, removeErr)
	bindings, err := store.pg.DB.NewSelect().Model((*dbmodel.EventRuleBinding)(nil)).Where("scope_id = ?", binding.Scope.ID).Count(ctx)
	require.NoError(t, err)
	require.Zero(t, bindings)
	_, err = store.GetByID(ctx, binding.RuleID)
	require.NoError(t, err, "removing a Rack must preserve its shared policy")
}

func waitForBlockedBindingQuery(t *testing.T, store *Store, pattern string, id uuid.UUID) {
	t.Helper()
	require.Eventually(t, func() bool {
		var blocked bool
		err := store.pg.DB.NewSelect().ColumnExpr(`EXISTS (
			SELECT 1 FROM pg_stat_activity
			WHERE datname = current_database() AND wait_event_type = 'Lock'
			AND query LIKE ? AND query LIKE ?
		)`, pattern, "%"+id.String()+"%").Scan(t.Context(), &blocked)
		return err == nil && blocked
	}, 5*time.Second, 10*time.Millisecond, "expected a blocked query matching %s for %s", pattern, id)
}
