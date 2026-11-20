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
	t.Run("reusable policy bindings", func(t *testing.T) { testReusablePolicyBindings(t, factory) })
	t.Run("binding deletion race", func(t *testing.T) { testBindingDeletionRace(t, factory) })
	t.Run("rule lifecycle", func(t *testing.T) {
		testRuleLifecycle(t, factory)
	})
	t.Run("binding invariants", func(t *testing.T) {
		testBindingInvariants(t, factory)
	})
	t.Run("concurrent binding conflict", func(t *testing.T) {
		testConcurrentBindingConflict(t, factory)
	})
	t.Run("delete requires no bindings", func(t *testing.T) {
		testDeleteRequiresNoBindings(t, factory)
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

	require.NoError(t, rules.SetEnabled(ctx, rule.ID, true))

	updated, err := rules.GetByID(ctx, rule.ID)
	require.NoError(t, err)
	assert.Equal(t, created.Name, updated.Name)
	assert.Equal(t, created.Description, updated.Description)
	assert.Equal(t, created.Policy, updated.Policy)
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

func testDeleteRequiresNoBindings(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	for _, test := range []struct {
		name    string
		enabled bool
	}{
		{name: "enabled policy", enabled: true},
		{name: "disabled policy"},
	} {
		t.Run(test.name, func(t *testing.T) {
			ctx := t.Context()
			rules, bindings := factory()
			rule := createRule(t, ctx, rules, "test.event")
			require.NoError(t, rules.SetEnabled(ctx, rule.ID, test.enabled))
			scope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
			binding := newBinding(rule, scope)
			require.NoError(t, bindings.Bind(ctx, binding))

			require.ErrorIs(t, rules.Delete(ctx, rule.ID), eventrule.ErrBindingConflict)
			retained, err := rules.GetByID(ctx, rule.ID)
			require.NoError(t, err)
			require.Equal(t, test.enabled, retained.Enabled)
			found, err := bindings.GetForScope(ctx, rule.EventType, scope)
			require.NoError(t, err)
			require.Equal(t, &binding, found)

			require.NoError(t, bindings.Unbind(ctx, rule.EventType, scope))
			require.NoError(t, rules.Delete(ctx, rule.ID))
			_, err = rules.GetByID(ctx, rule.ID)
			require.ErrorIs(t, err, eventrule.ErrRuleNotFound)
		})
	}
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

func testReusablePolicyBindings(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	rules, bindings := factory()
	ctx := t.Context()
	first, second := newRule("test.event"), newRule("test.event")
	_, err := rules.Create(ctx, first)
	require.NoError(t, err)
	_, err = rules.Create(ctx, second)
	require.NoError(t, err)
	site := eventrule.Scope{Type: eventrule.ScopeTypeSite}
	rack := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}
	for _, scope := range []eventrule.Scope{site, rack} {
		binding := eventrule.Binding{ID: uuid.New(), RuleID: first.ID, EventType: first.EventType, Scope: scope}
		saved, err := bindings.PutBinding(ctx, binding)
		require.NoError(t, err)
		binding.ID = uuid.New()
		repeated, err := bindings.PutBinding(ctx, binding)
		require.NoError(t, err)
		require.True(t, saved.Created)
		require.False(t, repeated.Created)
		require.Equal(t, saved.Binding.ID, repeated.Binding.ID)
	}
	rows, total, err := bindings.ListBindings(ctx, eventrule.BindingListRequest{RuleID: first.ID, Limit: 10})
	require.NoError(t, err)
	require.Equal(t, 2, total)
	require.Equal(t, site, rows[0].Scope)
	other := newRule("test.other")
	other.Enabled = true
	_, err = rules.Create(ctx, other)
	require.NoError(t, err)
	for _, scope := range []eventrule.Scope{site, rack} {
		_, err = bindings.PutBinding(ctx, newBinding(other, scope))
		require.NoError(t, err)
	}
	for _, test := range []struct {
		name  string
		query eventrule.BindingListRequest
		want  []string
		total int
	}{
		{name: "all event types and enabled states", want: []string{"site/test.event", "site/test.other", "rack/test.event", "rack/test.other"}, total: 4},
		{name: "Site scopes", query: eventrule.BindingListRequest{ScopeType: eventrule.ScopeTypeSite}, want: []string{"site/test.event", "site/test.other"}, total: 2},
		{name: "Rack scopes", query: eventrule.BindingListRequest{ScopeType: eventrule.ScopeTypeRack}, want: []string{"rack/test.event", "rack/test.other"}, total: 2},
		{name: "event type", query: eventrule.BindingListRequest{EventType: first.EventType}, want: []string{"site/test.event", "rack/test.event"}, total: 2},
		{name: "one Rack", query: eventrule.BindingListRequest{RackID: rack.ID}, want: []string{"rack/test.event", "rack/test.other"}, total: 2},
		{name: "filters combine", query: eventrule.BindingListRequest{EventType: other.EventType, ScopeType: eventrule.ScopeTypeRack, RuleID: other.ID}, want: []string{"rack/test.other"}, total: 1},
		{name: "unbound policy", query: eventrule.BindingListRequest{RuleID: second.ID}},
		{name: "unknown policy", query: eventrule.BindingListRequest{RuleID: uuid.New()}},
		{name: "unknown Rack", query: eventrule.BindingListRequest{RackID: uuid.New()}},
		{name: "stable page across event types", query: eventrule.BindingListRequest{Offset: 1, Limit: 2}, want: []string{"site/test.other", "rack/test.event"}, total: 4},
		{name: "empty page retains total", query: eventrule.BindingListRequest{Offset: 100}, total: 4},
	} {
		t.Run(test.name, func(t *testing.T) {
			query := test.query
			if query.Limit == 0 {
				query.Limit = 10
			}
			rows, total, err := bindings.ListBindings(ctx, query)
			require.NoError(t, err)
			require.Equal(t, test.total, total)
			keys := make([]string, 0, len(rows))
			for _, row := range rows {
				keys = append(keys, string(row.Scope.Type)+"/"+string(row.EventType))
			}
			require.Equal(t, len(test.want), len(keys))
			if len(test.want) > 0 {
				require.Equal(t, test.want, keys)
			}
		})
	}
	bound := true
	page, err := rules.List(ctx, eventrule.RuleListRequest{Filter: eventrule.RuleFilter{Bound: &bound, EventType: &first.EventType}, Limit: 10})
	require.NoError(t, err)
	require.Equal(t, 1, page.Total)
	require.False(t, page.Rules[0].Enabled)
	replacement, err := bindings.PutBinding(ctx, eventrule.Binding{ID: uuid.New(), RuleID: second.ID, EventType: second.EventType, Scope: rack})
	require.NoError(t, err)
	require.False(t, replacement.Created)
	require.Equal(t, rows[1].ID, replacement.Binding.ID)
	retained, err := bindings.GetForScope(ctx, first.EventType, site)
	require.NoError(t, err)
	require.Equal(t, first.ID, retained.RuleID)
	rows, total, err = bindings.ListBindings(ctx, eventrule.BindingListRequest{RuleID: second.ID, Offset: 100, Limit: 1})
	require.NoError(t, err)
	require.Empty(t, rows)
	require.Equal(t, 1, total)
	require.NoError(t, bindings.Unbind(ctx, first.EventType, site))
	require.NoError(t, rules.Delete(ctx, first.ID))
	_, err = rules.GetByID(ctx, second.ID)
	require.NoError(t, err)
}

func testBindingDeletionRace(t *testing.T, factory RuleBindingFactory) {
	t.Helper()
	rules, bindings := factory()
	ctx := t.Context()
	rule := newRule("test.event")
	_, err := rules.Create(ctx, rule)
	require.NoError(t, err)
	start := make(chan struct{})
	put := make(chan error, 1)
	deleted := make(chan error, 1)
	go func() {
		<-start
		_, err := bindings.PutBinding(ctx, eventrule.Binding{ID: uuid.New(), RuleID: rule.ID, EventType: rule.EventType, Scope: eventrule.Scope{Type: eventrule.ScopeTypeSite}})
		put <- err
	}()
	go func() { <-start; deleted <- rules.Delete(ctx, rule.ID) }()
	close(start)
	putErr, deleteErr := <-put, <-deleted
	if putErr == nil {
		require.ErrorIs(t, deleteErr, eventrule.ErrBindingConflict)
		_, err = rules.GetByID(ctx, rule.ID)
		require.NoError(t, err)
	} else {
		require.NoError(t, deleteErr)
		require.ErrorIs(t, putErr, eventrule.ErrRuleNotFound)
	}
}
