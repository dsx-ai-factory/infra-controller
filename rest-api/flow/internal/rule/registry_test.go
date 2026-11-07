// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRegistry_Register(t *testing.T) {
	t.Run("register and retrieve validator", func(t *testing.T) {
		registry := NewRegistry()
		validator := &testKindValidator{kind: "event"}

		require.NoError(t, registry.Register(validator))

		got, ok := registry.KindValidator("event")
		require.True(t, ok)
		assert.Same(t, validator, got)
		assert.Equal(t, []Kind{"event"}, registry.Kinds())
	})

	t.Run("kinds are sorted", func(t *testing.T) {
		registry := NewRegistry()

		require.NoError(t, registry.Register(&testKindValidator{kind: "operation"}))
		require.NoError(t, registry.Register(&testKindValidator{kind: "event"}))

		assert.Equal(t, []Kind{"event", "operation"}, registry.Kinds())
	})

	t.Run("duplicate kind is rejected", func(t *testing.T) {
		registry := NewRegistry()

		require.NoError(t, registry.Register(&testKindValidator{kind: "event"}))
		err := registry.Register(&testKindValidator{kind: "event"})

		require.Error(t, err)
		assert.ErrorContains(t, err, "already registered")
	})

	t.Run("invalid kind is rejected", func(t *testing.T) {
		registry := NewRegistry()

		err := registry.Register(&testKindValidator{kind: "Event"})

		require.Error(t, err)
		assert.ErrorContains(t, err, "kind")
	})

	t.Run("nil validator is rejected", func(t *testing.T) {
		registry := NewRegistry()

		err := registry.Register(nil)

		require.Error(t, err)
		assert.ErrorContains(t, err, "validator is nil")
	})
}

type testKindValidator struct {
	kind          Kind
	validate      func(*Rule) error
	validateCalls int
}

func (v *testKindValidator) Kind() Kind {
	return v.kind
}

func (v *testKindValidator) Validate(rule *Rule) error {
	v.validateCalls++
	if v.validate != nil {
		return v.validate(rule)
	}
	return nil
}
