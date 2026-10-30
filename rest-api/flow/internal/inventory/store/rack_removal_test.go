// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"database/sql"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
)

func TestPostgresStore_DeleteRack(t *testing.T) {
	testRackRemoval(t, false)
}

func TestPostgresStore_PurgeRack(t *testing.T) {
	testRackRemoval(t, true)
}

func testRackRemoval(t *testing.T, purge bool) {
	t.Helper()
	type testCase struct {
		name        string
		failCleanup bool
		keepActive  bool
	}
	cases := []testCase{
		{name: "removes bindings and preserves shared policies"},
		{name: "cleanup failure rolls back removal", failCleanup: true},
	}
	if purge {
		cases = append(cases, testCase{name: "active Rack cannot be purged", keepActive: true})
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := t.Context()
			session := storetest.NewPostgresTestSession(t)
			store := NewPostgres(session)
			target := &model.Rack{ID: uuid.New(), Name: "removed"}
			if purge && !tc.keepActive {
				now := time.Now().UTC()
				target.DeletedAt = &now
			}
			require.NoError(t, target.Create(ctx, session.DB))
			other := &model.Rack{ID: uuid.New(), Name: "retained"}
			require.NoError(t, other.Create(ctx, session.DB))
			component := &model.Component{ID: uuid.New(), RackID: target.ID, Name: "component", Type: "Compute"}
			require.NoError(t, component.Create(ctx, session.DB))
			policy, err := eventrulecodec.MarshalPolicy(eventrule.Policy{Actions: []eventrule.Action{{Name: "record", Spec: &eventrule.Noop{}}}})
			require.NoError(t, err)
			var protectedBinding uuid.UUID
			for _, enabled := range []bool{true, false} {
				eventType := "test.enabled"
				if !enabled {
					eventType = "test.disabled"
				}
				rule := &model.EventRule{ID: uuid.New(), Name: "shared", EventType: eventType, Enabled: enabled, Policy: policy}
				_, err = session.DB.NewInsert().Model(rule).Exec(ctx)
				require.NoError(t, err)
				bindings := []model.EventRuleBinding{
					{ID: uuid.New(), RuleID: rule.ID, EventType: rule.EventType, ScopeType: "rack", ScopeID: &target.ID},
					{ID: uuid.New(), RuleID: rule.ID, EventType: rule.EventType, ScopeType: "rack", ScopeID: &other.ID},
					{ID: uuid.New(), RuleID: rule.ID, EventType: rule.EventType, ScopeType: "site"},
				}
				_, err = session.DB.NewInsert().Model(&bindings).Exec(ctx)
				require.NoError(t, err)
				protectedBinding = bindings[0].ID
			}
			if tc.failCleanup {
				_, err = session.DB.ExecContext(ctx, "CREATE TABLE binding_cleanup_guard (binding_id uuid REFERENCES event_rule_bindings(id))")
				require.NoError(t, err)
				_, err = session.DB.ExecContext(ctx, "INSERT INTO binding_cleanup_guard VALUES (?)", protectedBinding)
				require.NoError(t, err)
			}

			if purge {
				err = store.PurgeRack(ctx, target.ID)
			} else {
				err = store.DeleteRack(ctx, target.ID)
			}
			failed := tc.failCleanup || tc.keepActive
			if tc.failCleanup {
				require.Equal(t, codes.Internal, status.Code(err))
				require.ErrorContains(t, err, "binding_cleanup_guard")
				require.ErrorContains(t, err, "SQLSTATE 23503")
			} else if tc.keepActive {
				require.ErrorContains(t, err, "not soft-deleted")
			} else {
				require.NoError(t, err)
			}
			remaining, err := session.DB.NewSelect().Model((*model.EventRuleBinding)(nil)).Where("scope_type = 'rack' AND scope_id = ?", target.ID).Count(ctx)
			require.NoError(t, err)
			if failed {
				require.Equal(t, 2, remaining)
			} else {
				require.Zero(t, remaining)
			}
			retained, err := session.DB.NewSelect().Model((*model.EventRuleBinding)(nil)).Where("scope_type = 'site' OR scope_id = ?", other.ID).Count(ctx)
			require.NoError(t, err)
			require.Equal(t, 4, retained)
			rules, err := session.DB.NewSelect().Model((*model.EventRule)(nil)).Count(ctx)
			require.NoError(t, err)
			require.Equal(t, 2, rules)
			rack, rackErr := target.GetIncludingDeleted(ctx, session.DB)
			var storedComponent model.Component
			componentErr := session.DB.NewSelect().Model(&storedComponent).WhereAllWithDeleted().Where("c.id = ?", component.ID).Scan(ctx)
			if purge && !failed {
				require.ErrorIs(t, rackErr, sql.ErrNoRows)
				require.ErrorIs(t, componentErr, sql.ErrNoRows)
			} else {
				require.NoError(t, rackErr)
				require.NoError(t, componentErr)
				if failed {
					require.Equal(t, target.DeletedAt == nil, rack.DeletedAt == nil)
					require.Nil(t, storedComponent.DeletedAt)
				} else {
					require.NotNil(t, rack.DeletedAt)
					require.NotNil(t, storedComponent.DeletedAt)
				}
			}
		})
	}
}
