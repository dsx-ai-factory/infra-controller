// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestBuiltInRegistryRejectsInvalidAndDuplicateRules(t *testing.T) {
	valid := testRule(uuid.New(), "test.event")
	duplicateID := testRule(valid.ID, "other.event")
	duplicateType := testRule(uuid.New(), valid.EventType)
	persisted := testRule(uuid.New(), "persisted.event")
	persisted.Origin = eventrule.RuleOriginPersisted
	invalid := testRule(uuid.New(), "invalid.event")
	invalid.Actions = nil

	tests := map[string]struct {
		rules   []*eventrule.Rule
		wantErr string
	}{
		"invalid origin":       {rules: []*eventrule.Rule{persisted}, wantErr: "origin must be"},
		"invalid policy":       {rules: []*eventrule.Rule{invalid}, wantErr: "actions are required"},
		"duplicate id":         {rules: []*eventrule.Rule{valid, duplicateID}, wantErr: "duplicate id"},
		"duplicate event type": {rules: []*eventrule.Rule{valid, duplicateType}, wantErr: "duplicate event type"},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := testBuiltInRegistry(test.rules...)
			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestBuiltInRegistryLookup(t *testing.T) {
	rule := testRule(uuid.New(), "test.event")
	registry, err := testBuiltInRegistry(rule)
	require.NoError(t, err)

	rule.Actions[0].Name = "changed-after-registration"

	byID, err := registry.GetByID(context.Background(), rule.ID)
	require.NoError(t, err)
	require.Equal(t, rule.ID, byID.ID)
	require.Equal(t, "noop", byID.Actions[0].Name)
	byID.Actions[0].Name = "changed"

	byType, err := registry.GetByEventType(context.Background(), rule.EventType)
	require.NoError(t, err)
	require.Equal(t, rule.ID, byType.ID)
	require.Equal(t, "noop", byType.Actions[0].Name)
}

func TestBuiltInRegistryGetByEventTypeDetectsInconsistentState(t *testing.T) {
	eventType := eventrule.Type("test.event")
	registry := &builtInRegistry{
		byID:        make(map[uuid.UUID]eventrule.Rule),
		byEventType: map[eventrule.Type]uuid.UUID{eventType: uuid.New()},
	}

	_, err := registry.GetByEventType(context.Background(), eventType)
	require.ErrorContains(t, err, "registry is inconsistent")
	require.False(t, errors.Is(err, eventrule.ErrRuleNotFound))
}

func TestBuiltInRegistry_supportsEventType(t *testing.T) {
	rule := testRule(uuid.New(), "test.event")
	registry, err := testBuiltInRegistry(rule)
	require.NoError(t, err)

	tests := map[string]struct {
		eventType eventrule.Type
		want      bool
	}{
		"registered": {
			eventType: rule.EventType,
			want:      true,
		},
		"unregistered": {
			eventType: "other.event",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, registry.supportsEventType(test.eventType))
		})
	}
}

func TestBuiltInRegistry_List(t *testing.T) {
	first := testRule(
		uuid.MustParse("00000000-0000-0000-0000-000000000001"),
		"first.event",
	)
	second := testRule(
		uuid.MustParse("00000000-0000-0000-0000-000000000002"),
		"second.event",
	)
	third := testRule(
		uuid.MustParse("00000000-0000-0000-0000-000000000003"),
		"third.event",
	)
	registry, err := testBuiltInRegistry(third, first, second)
	require.NoError(t, err)

	page, err := registry.List(context.Background(), eventrule.RuleListRequest{
		Offset: 1,
		Limit:  1,
	})

	require.NoError(t, err)
	require.Equal(t, 3, page.Total)
	require.Len(t, page.Rules, 1)
	require.Equal(t, second.ID, page.Rules[0].ID)
}

func testBuiltInRegistry(rules ...*eventrule.Rule) (*builtInRegistry, error) {
	registry := &builtInRegistry{
		byID:        make(map[uuid.UUID]eventrule.Rule, len(rules)),
		byEventType: make(map[eventrule.Type]uuid.UUID, len(rules)),
	}

	for _, rule := range rules {
		if err := registry.addRule(rule); err != nil {
			return nil, err
		}
	}

	return registry, nil
}

func testRule(id uuid.UUID, eventType eventrule.Type) *eventrule.Rule {
	return &eventrule.Rule{
		ID:        id,
		Origin:    eventrule.RuleOriginBuiltIn,
		Name:      "test",
		Enabled:   true,
		EventType: eventType,
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	}
}
