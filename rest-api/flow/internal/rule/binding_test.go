// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestBinding_Validate(t *testing.T) {
	ruleID := uuid.New()
	rackID := uuid.New()

	t.Run("valid global binding", func(t *testing.T) {
		binding := &Binding{
			RuleID: ruleID,
			Scope: Scope{
				ScopeType: ScopeTypeGlobal,
			},
			Priority: 100,
		}

		require.NoError(t, binding.Validate())
	})

	t.Run("valid rack binding", func(t *testing.T) {
		binding := &Binding{
			RuleID: ruleID,
			Scope: Scope{
				ScopeType: ScopeTypeRack,
				ScopeID:   &rackID,
			},
			Selector: Selector{"operation_type": "power_control"},
			Priority: 100,
		}

		require.NoError(t, binding.Validate())
	})

	t.Run("nil binding", func(t *testing.T) {
		var binding *Binding

		err := binding.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "binding is nil")
	})

	t.Run("missing rule id", func(t *testing.T) {
		binding := &Binding{
			Scope: Scope{
				ScopeType: ScopeTypeGlobal,
			},
		}

		err := binding.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "rule_id is required")
	})

	t.Run("global scope cannot have scope id", func(t *testing.T) {
		binding := &Binding{
			RuleID: ruleID,
			Scope: Scope{
				ScopeType: ScopeTypeGlobal,
				ScopeID:   &rackID,
			},
		}

		err := binding.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "global binding must not have a scope_id")
	})

	t.Run("rack scope requires scope id", func(t *testing.T) {
		binding := &Binding{
			RuleID: ruleID,
			Scope: Scope{
				ScopeType: ScopeTypeRack,
			},
		}

		err := binding.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "requires a scope_id")
	})

	t.Run("unknown scope type", func(t *testing.T) {
		binding := &Binding{
			RuleID: ruleID,
			Scope: Scope{
				ScopeType: "site",
			},
		}

		err := binding.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "unknown scope_type")
	})

	t.Run("negative priority", func(t *testing.T) {
		binding := &Binding{
			RuleID: ruleID,
			Scope: Scope{
				ScopeType: ScopeTypeGlobal,
			},
			Priority: -1,
		}

		err := binding.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "priority")
	})
}
