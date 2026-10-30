// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package memory

import (
	"context"
	"testing"
	"time"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestStoreContract(t *testing.T) {
	storetest.RunRuleBindingContract(t, func() (eventrule.RuleStore, eventrule.BindingStore) {
		store := New()

		return store, store
	})

	storetest.RunExecutionContract(t, func(now *time.Time) storetest.EventExecutionStore {
		return NewWithClock(func() time.Time { return *now })
	})
}

func TestBindingScansIgnoreUnrelatedInvalidRecords(t *testing.T) {
	ctx := context.Background()
	store := New()
	invalidID := uuid.New()
	store.bindings[invalidID] = dbmodel.EventRuleBinding{
		ID:        invalidID,
		RuleID:    uuid.New(),
		EventType: "invalid",
		ScopeType: "invalid",
	}

	rule, err := store.Create(ctx, &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    eventrule.RuleOriginPersisted,
		Name:      "test",
		EventType: "test.event",
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	})
	require.NoError(t, err)

	scope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	binding := eventrule.Binding{
		ID:        uuid.New(),
		RuleID:    rule.ID,
		EventType: rule.EventType,
		Scope:     scope,
	}
	require.NoError(t, store.Bind(ctx, binding))

	found, err := store.GetForScope(ctx, rule.EventType, scope)
	require.NoError(t, err)
	require.Equal(t, &binding, found)
	require.NoError(t, store.Delete(ctx, rule.ID))
}

func TestStore_ScopeMutationDoesNotChangeOtherBindings(t *testing.T) {
	ctx := t.Context()
	store := New()
	eventType := eventrule.Type("test.event")
	original, err := store.Create(ctx, &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    eventrule.RuleOriginPersisted,
		Name:      "shared rule",
		Enabled:   true,
		EventType: eventType,
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	})
	require.NoError(t, err)
	firstScope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	secondScope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	for _, scope := range []eventrule.Scope{firstScope, secondScope} {
		require.NoError(t, store.Bind(ctx, eventrule.Binding{
			ID: uuid.New(), RuleID: original.ID, EventType: eventType, Scope: scope,
		}))
	}

	replacement := &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    eventrule.RuleOriginPersisted,
		Name:      "replacement",
		Enabled:   true,
		EventType: eventType,
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	}
	replaced, err := store.SetRuleOverride(ctx, replacement, eventrule.Binding{
		ID: uuid.New(), RuleID: replacement.ID, EventType: eventType, Scope: firstScope,
	})
	require.NoError(t, err)
	require.Equal(t, replacement.ID, replaced.ID)

	firstBinding, err := store.GetForScope(ctx, eventType, firstScope)
	require.NoError(t, err)
	require.Equal(t, replacement.ID, firstBinding.RuleID)
	secondBinding, err := store.GetForScope(ctx, eventType, secondScope)
	require.NoError(t, err)
	require.Equal(t, original.ID, secondBinding.RuleID)
	unchanged, err := store.GetByID(ctx, original.ID)
	require.NoError(t, err)
	require.Equal(t, "shared rule", unchanged.Name)

	require.NoError(t, store.DeleteRuleOverride(ctx, eventType, firstScope))
	_, err = store.GetByID(ctx, replacement.ID)
	require.ErrorIs(t, err, eventrule.ErrRuleNotFound)
	_, err = store.GetByID(ctx, original.ID)
	require.NoError(t, err)
}

func TestStore_CommitEventPlanRejectsDanglingIndexes(t *testing.T) {
	now := time.Date(2026, 8, 7, 12, 0, 0, 0, time.UTC)
	store := NewWithClock(func() time.Time { return now })
	definition := eventrule.Event{
		Key:           eventrule.EventKey{SourceName: "test", SourceKey: "event-1"},
		Type:          "test.event",
		Resource:      eventrule.ResourceIdentity{Kind: eventrule.ResourceKindRack, ID: uuid.New()},
		AppliedRuleID: uuid.New(),
		EffectivePolicy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "action", Spec: &eventrule.Noop{}},
		}},
		Summary: "Test event",
	}

	store.eventsByKey[definition.Key] = uuid.New()

	event, err := store.CommitEventPlan(context.Background(), definition, []eventrule.PlannedExecution{{
		ActionName:    "action",
		ExecutionPlan: &eventrule.NoopPlan{},
	}})

	require.ErrorIs(t, err, eventrule.ErrEventNotFound)
	require.Nil(t, event)
}
