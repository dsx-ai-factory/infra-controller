// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"testing"
	"time"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestEventRuleRoundTrip(t *testing.T) {
	now := time.Now().UTC()
	local := time.FixedZone("PDT", -7*60*60)
	rule := &eventrule.Rule{
		ID:          uuid.New(),
		Origin:      eventrule.RuleOriginPersisted,
		Name:        "test",
		Description: "description",
		Enabled:     true,
		EventType:   "test.event",
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
		CreatedAt: now,
		UpdatedAt: now,
	}

	dbRule, err := EventRuleTo(rule)
	require.NoError(t, err)
	dbRule.CreatedAt = dbRule.CreatedAt.In(local)
	dbRule.UpdatedAt = dbRule.UpdatedAt.In(local)
	roundTripped, err := EventRuleFrom(dbRule)
	require.NoError(t, err)
	require.Equal(t, rule, roundTripped)
}

func TestEventRuleFromRejectsInvalidModel(t *testing.T) {
	tests := map[string]struct {
		mutate      func(*dbmodel.EventRule)
		wantMessage string
	}{
		"invalid aggregate": {
			mutate: func(rule *dbmodel.EventRule) {
				rule.Name = ""
			},
			wantMessage: "event rule name is empty",
		},
		"invalid policy": {
			mutate: func(rule *dbmodel.EventRule) {
				rule.Policy = []byte(`{"version": 999}`)
			},
			wantMessage: "decode policy",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			dbRule, err := EventRuleTo(&eventrule.Rule{
				ID:        uuid.New(),
				Origin:    eventrule.RuleOriginPersisted,
				Name:      "test",
				EventType: "test.event",
				Policy: eventrule.Policy{Actions: []eventrule.Action{
					{Name: "noop", Spec: &eventrule.Noop{}},
				}},
			})
			require.NoError(t, err)
			test.mutate(dbRule)

			_, err = EventRuleFrom(dbRule)
			require.ErrorIs(t, err, eventrule.ErrInvalidPersistedRule)
			require.ErrorContains(t, err, test.wantMessage)
		})
	}
}

func TestEventRuleBindingRoundTrip(t *testing.T) {
	scopes := map[string]eventrule.Scope{
		"site": {Type: eventrule.ScopeTypeSite},
		"rack": {Type: eventrule.ScopeTypeRack, ID: uuid.New()},
	}
	for name, scope := range scopes {
		t.Run(name, func(t *testing.T) {
			binding := eventrule.Binding{
				ID:        uuid.New(),
				RuleID:    uuid.New(),
				EventType: "test.event",
				Scope:     scope,
			}
			now := time.Now().UTC()
			dbBinding, err := EventRuleBindingTo(binding, now, now)
			require.NoError(t, err)
			roundTripped, err := EventRuleBindingFrom(dbBinding)
			require.NoError(t, err)
			require.Equal(t, &binding, roundTripped)
		})
	}
}

func TestEventRuleBindingFromRejectsInvalidModel(t *testing.T) {
	dbBinding := &dbmodel.EventRuleBinding{
		ID:        uuid.New(),
		RuleID:    uuid.New(),
		EventType: "test.event",
		ScopeType: string(eventrule.ScopeTypeRack),
	}
	_, err := EventRuleBindingFrom(dbBinding)
	require.ErrorContains(t, err, "rack scope requires an id")
}
