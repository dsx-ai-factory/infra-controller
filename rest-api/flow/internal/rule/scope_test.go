// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestScope_Validate(t *testing.T) {
	rackID := uuid.New()

	t.Run("global scope has no scope id", func(t *testing.T) {
		scope := Scope{
			ScopeType: ScopeTypeGlobal,
		}

		require.NoError(t, scope.Validate())
	})

	t.Run("rack scope uses scope id", func(t *testing.T) {
		scope := Scope{
			ScopeType: ScopeTypeRack,
			ScopeID:   &rackID,
		}

		require.NoError(t, scope.Validate())
	})

	t.Run("global scope rejects scope id", func(t *testing.T) {
		scope := Scope{
			ScopeType: ScopeTypeGlobal,
			ScopeID:   &rackID,
		}

		err := scope.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "global binding must not have a scope_id")
	})

	t.Run("non-global scope requires scope id", func(t *testing.T) {
		scope := Scope{
			ScopeType: ScopeTypeRack,
		}

		err := scope.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "requires a scope_id")
	})
}
