// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package storetest contains reusable event-rule store contract tests.
package storetest

import (
	"context"
	"slices"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// RuleBindingFactory constructs empty rule and binding stores that share one
// persistence boundary.
type RuleBindingFactory func() (eventrule.RuleStore, eventrule.BindingStore)

// RunRuleBindingContract executes the shared rule and binding store contract.
func RunRuleBindingContract(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	t.Run("rule lifecycle", func(t *testing.T) {
		testRuleLifecycle(t, factory)
	})
	t.Run("binding invariants", func(t *testing.T) {
		testBindingInvariants(t, factory)
	})
	t.Run("concurrent binding conflict", func(t *testing.T) {
		testConcurrentBindingConflict(t, factory)
	})
	t.Run("delete cascades bindings", func(t *testing.T) {
		testDeleteCascadesBindings(t, factory)
	})
}

func testRuleLifecycle(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	ctx := context.Background()
	rules, _ := factory()
	rule := newRule("test.event")

	created, err := rules.Create(ctx, rule)
	require.NoError(t, err)
	require.NotZero(t, created.CreatedAt)
	assert.Equal(t, created.CreatedAt, created.UpdatedAt)
	_, err = rules.Create(ctx, rule)
	require.Error(t, err)

	invalid := newRule("test.invalid")
	invalid.Name = ""
	_, err = rules.Create(ctx, invalid)
	require.Error(t, err)
	_, err = rules.GetByID(ctx, invalid.ID)
	require.ErrorIs(t, err, eventrule.ErrRuleNotFound)

	loaded, err := rules.GetByID(ctx, rule.ID)
	require.NoError(t, err)
	require.Equal(t, created, loaded)
	loaded.Name = "caller mutation"
	reloaded, err := rules.GetByID(ctx, rule.ID)
	require.NoError(t, err)
	assert.Equal(t, "test", reloaded.Name)

	require.NoError(t, rules.UpdateMetadata(ctx, rule.ID, eventrule.RuleMetadata{
		Name:        "updated",
		Description: "updated description",
	}))
	require.NoError(t, rules.ReplaceActions(ctx, rule.ID, []eventrule.Action{
		{Name: "replacement", Spec: &eventrule.Noop{}},
	}))
	require.NoError(t, rules.SetEnabled(ctx, rule.ID, true))

	updated, err := rules.GetByID(ctx, rule.ID)
	require.NoError(t, err)
	assert.Equal(t, "updated", updated.Name)
	assert.Equal(t, "updated description", updated.Description)
	assert.Equal(t, "replacement", updated.Actions[0].Name)
	assert.True(t, updated.Enabled)
	assert.False(t, updated.UpdatedAt.Before(updated.CreatedAt))
	require.NoError(t, rules.SetEnabled(ctx, rule.ID, true))
	unchanged, err := rules.GetByID(ctx, rule.ID)
	require.NoError(t, err)
	assert.Equal(t, updated.UpdatedAt, unchanged.UpdatedAt)

	enabled := true
	listed, err := rules.List(ctx, eventrule.RuleListRequest{
		Filter: eventrule.RuleFilter{
			EventType: &rule.EventType,
			Enabled:   &enabled,
		},
		Limit: 100,
	})
	require.NoError(t, err)
	require.Equal(t, 1, listed.Total)
	require.Len(t, listed.Rules, 1)
	assert.Equal(t, rule.ID, listed.Rules[0].ID)

	second := createRule(t, ctx, rules, rule.EventType)
	third := createRule(t, ctx, rules, rule.EventType)
	orderedIDs := []string{rule.ID.String(), second.ID.String(), third.ID.String()}
	slices.Sort(orderedIDs)
	page, err := rules.List(ctx, eventrule.RuleListRequest{Offset: 1, Limit: 1})
	require.NoError(t, err)
	require.Equal(t, 3, page.Total)
	require.Len(t, page.Rules, 1)
	assert.Equal(t, orderedIDs[1], page.Rules[0].ID.String())

	_, err = rules.List(ctx, eventrule.RuleListRequest{})
	require.ErrorContains(t, err, "limit must be positive")

	require.NoError(t, rules.Delete(ctx, rule.ID))
	_, err = rules.GetByID(ctx, rule.ID)
	require.ErrorIs(t, err, eventrule.ErrRuleNotFound)

	unknownID := uuid.New()
	mutations := map[string]func() error{
		"update metadata": func() error {
			return rules.UpdateMetadata(
				ctx,
				unknownID,
				eventrule.RuleMetadata{Name: "updated"},
			)
		},
		"replace actions": func() error {
			return rules.ReplaceActions(ctx, unknownID, []eventrule.Action{
				{Name: "noop", Spec: &eventrule.Noop{}},
			})
		},
		"delete": func() error {
			return rules.Delete(ctx, unknownID)
		},
		"set enabled": func() error {
			return rules.SetEnabled(ctx, unknownID, true)
		},
	}
	for name, mutate := range mutations {
		t.Run("unknown ID "+name, func(t *testing.T) {
			require.ErrorIs(t, mutate(), eventrule.ErrRuleNotFound)
		})
	}
}

func testBindingInvariants(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	ctx := context.Background()
	rules, bindings := factory()
	first := createRule(t, ctx, rules, "test.event")
	second := createRule(t, ctx, rules, "test.event")
	rackID := uuid.New()
	firstRack := newBinding(first, eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID})
	require.NoError(t, bindings.Bind(ctx, firstRack))

	found, err := bindings.GetForScope(ctx, first.EventType, firstRack.Scope)
	require.NoError(t, err)
	require.Equal(t, &firstRack, found)

	missing, err := bindings.GetForScope(
		ctx,
		first.EventType,
		eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()},
	)
	require.NoError(t, err)
	assert.Nil(t, missing)

	require.Error(t, bindings.Bind(ctx, newBinding(
		second,
		eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID},
	)))
	require.Error(t, bindings.Bind(ctx, newBinding(
		first,
		eventrule.Scope{Type: eventrule.ScopeTypeSite},
	)))

	secondSite := newBinding(second, eventrule.Scope{Type: eventrule.ScopeTypeSite})
	require.NoError(t, bindings.Bind(ctx, secondSite))
	require.ErrorContains(t, bindings.Bind(ctx, newBinding(
		first,
		eventrule.Scope{Type: eventrule.ScopeTypeSite},
	)), "already has a binding for scope")
	require.ErrorContains(t, bindings.Bind(ctx, newBinding(
		second,
		eventrule.Scope{Type: eventrule.ScopeTypeSite},
	)), "already has a binding for scope")
	require.ErrorContains(t, bindings.Bind(ctx, newBinding(
		second,
		eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()},
	)), "cannot mix site and rack bindings")

	mismatched := newBinding(first, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   uuid.New(),
	})
	mismatched.EventType = "other.event"
	require.Error(t, bindings.Bind(ctx, mismatched))

	require.NoError(t, bindings.Unbind(ctx, first.EventType, firstRack.Scope))
	require.ErrorIs(
		t,
		bindings.Unbind(ctx, first.EventType, firstRack.Scope),
		eventrule.ErrBindingNotFound,
	)
	found, err = bindings.GetForScope(ctx, first.EventType, firstRack.Scope)
	require.NoError(t, err)
	assert.Nil(t, found)

	require.NoError(t, bindings.Unbind(ctx, second.EventType, secondSite.Scope))
	found, err = bindings.GetForScope(ctx, second.EventType, secondSite.Scope)
	require.NoError(t, err)
	assert.Nil(t, found)
}

func testConcurrentBindingConflict(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	ctx := context.Background()
	rules, bindings := factory()
	first := createRule(t, ctx, rules, "test.event")
	second := createRule(t, ctx, rules, "test.event")
	scope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	candidates := []eventrule.Binding{
		newBinding(first, scope),
		newBinding(second, scope),
	}

	results := make(chan error, len(candidates))
	for _, binding := range candidates {
		go func() {
			results <- bindings.Bind(ctx, binding)
		}()
	}

	successes := 0
	for range candidates {
		if err := <-results; err == nil {
			successes++
		}
	}
	assert.Equal(t, 1, successes)
}

func testDeleteCascadesBindings(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	ctx := context.Background()
	rules, bindings := factory()
	rule := createRule(t, ctx, rules, "test.event")
	scope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	require.NoError(t, bindings.Bind(ctx, newBinding(rule, scope)))

	require.NoError(t, rules.Delete(ctx, rule.ID))
	found, err := bindings.GetForScope(ctx, rule.EventType, scope)
	require.NoError(t, err)
	assert.Nil(t, found)
}

func createRule(
	t *testing.T,
	ctx context.Context,
	store eventrule.RuleStore,
	eventType eventrule.Type,
) *eventrule.Rule {
	t.Helper()
	created, err := store.Create(ctx, newRule(eventType))
	require.NoError(t, err)
	return created
}

func newRule(eventType eventrule.Type) *eventrule.Rule {
	return &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    eventrule.RuleOriginPersisted,
		Name:      "test",
		EventType: eventType,
		Policy: eventrule.Policy{
			Actions: []eventrule.Action{
				{Name: "noop", Spec: &eventrule.Noop{}},
			},
		},
	}
}

func newBinding(rule *eventrule.Rule, scope eventrule.Scope) eventrule.Binding {
	return eventrule.Binding{
		ID:        uuid.New(),
		RuleID:    rule.ID,
		EventType: rule.EventType,
		Scope:     scope,
	}
}
