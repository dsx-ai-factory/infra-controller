// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// mockRuleStore implements RuleStore for testing.
type mockRuleStore struct {
	rules         map[uuid.UUID]*OperationRule
	rackAssocRule *OperationRule
	rackAssocErr  error
	getByIDErr    error
}

func newMockRuleStore() *mockRuleStore {
	return &mockRuleStore{
		rules: make(map[uuid.UUID]*OperationRule),
	}
}

func (m *mockRuleStore) GetRule(_ context.Context, id uuid.UUID) (*OperationRule, error) {
	if m.getByIDErr != nil {
		return nil, m.getByIDErr
	}
	rule, ok := m.rules[id]
	if !ok {
		return nil, nil
	}
	return rule, nil
}

func (m *mockRuleStore) GetRuleByOperationAndRack(
	_ context.Context, _ common.TaskType, _ string, _ *uuid.UUID,
) (*OperationRule, error) {
	if m.rackAssocErr != nil {
		return nil, m.rackAssocErr
	}
	return m.rackAssocRule, nil
}

func (m *mockRuleStore) addRule(rule *OperationRule) {
	m.rules[rule.ID] = rule
}

func TestResolveRule_ExplicitRuleID(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	resolver := NewResolver(store)

	ruleID := uuid.New()
	rackID := uuid.New()
	explicitRule := &OperationRule{
		ID:            ruleID,
		Name:          "Custom Power On Rule",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequencePowerOn,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps:   []SequenceStep{},
		},
	}
	store.addRule(explicitRule)

	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOn, rackID, &ruleID)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, ruleID, rule.ID)
	assert.Equal(t, "Custom Power On Rule", rule.Name)
}

func TestResolveRule_ExplicitRuleID_NotFound(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	resolver := NewResolver(store)

	missingID := uuid.New()
	rackID := uuid.New()

	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOn, rackID, &missingID)

	assert.Error(t, err)
	assert.Nil(t, rule)
	assert.Contains(t, err.Error(), "not found")
}

func TestResolveRule_ExplicitRuleID_StoreError(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	store.getByIDErr = fmt.Errorf("database connection lost")
	resolver := NewResolver(store)

	ruleID := uuid.New()
	rackID := uuid.New()

	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOn, rackID, &ruleID)

	assert.Error(t, err)
	assert.Nil(t, rule)
	assert.Contains(t, err.Error(), "database connection lost")
}

func TestResolveRule_ExplicitRuleID_OverridesRackAssociation(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	resolver := NewResolver(store)

	rackID := uuid.New()

	// Rack has an associated rule
	rackRule := &OperationRule{
		ID:   uuid.New(),
		Name: "Rack Association Rule",
	}
	store.rackAssocRule = rackRule

	// Caller requests a different explicit rule
	explicitID := uuid.New()
	explicitRule := &OperationRule{
		ID:   explicitID,
		Name: "Explicit Override Rule",
	}
	store.addRule(explicitRule)

	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOn, rackID, &explicitID)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, explicitID, rule.ID)
	assert.Equal(t, "Explicit Override Rule", rule.Name)
}

func TestResolveRule_NilRuleID_FallsThrough(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	resolver := NewResolver(store)

	rackID := uuid.New()

	// Rack has an associated rule
	rackRule := &OperationRule{
		ID:   uuid.New(),
		Name: "Rack Association Rule",
	}
	store.rackAssocRule = rackRule

	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOn, rackID, nil)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, "Rack Association Rule", rule.Name)
}

func TestResolveRule_ZeroRuleID_FallsThrough(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	resolver := NewResolver(store)

	rackID := uuid.New()
	zeroID := uuid.Nil

	// No rack association → falls through to hardcoded default
	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOn, rackID, &zeroID)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, "Hardcoded Default Power On", rule.Name)
}

func TestResolveRule_NilRuleID_NoRackAssoc_FallsToHardcoded(t *testing.T) {
	ctx := context.Background()
	store := newMockRuleStore()
	resolver := NewResolver(store)

	rackID := uuid.New()

	rule, err := resolver.ResolveRule(ctx, common.TaskTypePowerControl, SequencePowerOff, rackID, nil)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, "Hardcoded Default Power Off", rule.Name)
}

func TestResolveRule_ColdResetFallsToComputeOnlyHardcodedRule(t *testing.T) {
	resolver := NewResolver(newMockRuleStore())

	rule, err := resolver.ResolveRule(
		context.Background(),
		common.TaskTypePowerControl,
		SequenceColdReset,
		uuid.New(),
		nil,
	)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, "Hardcoded Default AC Power Cycle", rule.Name)
	assert.Equal(t, SequenceColdReset, rule.OperationCode)
	require.Len(t, rule.RuleDefinition.Steps, 1)
	assert.Equal(t, devicetypes.ComponentTypeCompute, rule.RuleDefinition.Steps[0].ComponentType)
	assert.Equal(t, ActionPowerControl, rule.RuleDefinition.Steps[0].MainOperation.Name)
}

func TestResolveRule_NilResolver_IgnoresRuleID(t *testing.T) {
	var resolver *Resolver

	rackID := uuid.New()
	ruleID := uuid.New()

	// Nil resolver ignores explicit rule_id and returns hardcoded default
	rule, err := resolver.ResolveRule(context.Background(), common.TaskTypePowerControl, SequencePowerOn, rackID, &ruleID)

	require.NoError(t, err)
	require.NotNil(t, rule)
	assert.Equal(t, "Hardcoded Default Power On", rule.Name)
}
