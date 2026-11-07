// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRule_Validate(t *testing.T) {
	t.Run("valid rule", func(t *testing.T) {
		registry, validator := validRuleRegistry(t)
		rule := validRule()

		require.NoError(t, rule.Validate(registry))
		assert.Equal(t, 1, validator.validateCalls)
	})

	t.Run("invalid kind", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.Kind = "Event"

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "kind")
	})

	t.Run("missing name", func(t *testing.T) {
		registry, validator := validRuleRegistry(t)
		rule := validRule()
		rule.Name = " "

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "rule name is empty")
		assert.Zero(t, validator.validateCalls)
	})

	t.Run("negative priority", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.Priority = -1

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "priority")
	})

	t.Run("missing schema version", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.SchemaVersion = ""

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "schema_version is empty")
	})

	t.Run("invalid selector", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.Selector = Selector{"bad": func() {}}

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "selector must be JSON serializable")
	})

	t.Run("missing definition", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.Definition = nil

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "definition is required")
	})

	t.Run("null definition", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.Definition = json.RawMessage(`null`)

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "definition cannot be null")
	})

	t.Run("invalid definition JSON", func(t *testing.T) {
		registry, _ := validRuleRegistry(t)
		rule := validRule()
		rule.Definition = json.RawMessage(`{"version":`)

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "definition must be valid JSON")
	})

	t.Run("rejects nil registry after generic validation", func(t *testing.T) {
		rule := validRule()

		err := rule.Validate(nil)

		require.Error(t, err)
		assert.ErrorContains(t, err, "rule registry is nil")
	})

	t.Run("rejects unregistered kind", func(t *testing.T) {
		registry := NewRegistry()
		rule := validRule()

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "not registered")
	})

	t.Run("wraps kind validation error", func(t *testing.T) {
		registry, _ := validRuleRegistryWithValidator(t, &testKindValidator{
			kind: "test",
			validate: func(_ *Rule) error {
				return fmt.Errorf("missing match")
			},
		})
		rule := validRule()

		err := rule.Validate(registry)

		require.Error(t, err)
		assert.ErrorContains(t, err, "invalid test rule")
		assert.ErrorContains(t, err, "missing match")
	})
}

func TestRule_ValidateEnvelope(t *testing.T) {
	t.Run("validates generic envelope without registry", func(t *testing.T) {
		rule := validRule()
		rule.Kind = "unregistered"

		require.NoError(t, rule.ValidateEnvelope())
	})

	t.Run("rejects invalid generic envelope", func(t *testing.T) {
		rule := validRule()
		rule.Name = ""

		err := rule.ValidateEnvelope()

		require.Error(t, err)
		assert.ErrorContains(t, err, "rule name is empty")
	})
}

func validRule() *Rule {
	return &Rule{
		Kind:          "test",
		SchemaVersion: "v1",
		Definition:    json.RawMessage(`{"version":"v1"}`),
		Name:          "Test rule",
		Selector:      Selector{"event_type": "hardware.leak.detected"},
		Enabled:       true,
		Priority:      100,
	}
}

func validRuleRegistry(t *testing.T) (*Registry, *testKindValidator) {
	t.Helper()
	return validRuleRegistryWithValidator(t, &testKindValidator{kind: "test"})
}

func validRuleRegistryWithValidator(t *testing.T, validator *testKindValidator) (*Registry, *testKindValidator) {
	t.Helper()

	registry := NewRegistry()
	require.NoError(t, registry.Register(validator))

	return registry, validator
}
