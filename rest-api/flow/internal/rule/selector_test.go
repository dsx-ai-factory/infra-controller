// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSelector_Validate(t *testing.T) {
	t.Run("nil selector is valid", func(t *testing.T) {
		var selector Selector

		require.NoError(t, selector.Validate())
	})

	t.Run("empty key is invalid", func(t *testing.T) {
		selector := Selector{"": "value"}

		err := selector.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "selector key is empty")
	})

	t.Run("key with surrounding whitespace is invalid", func(t *testing.T) {
		selector := Selector{" event_type ": "hardware.leak.detected"}

		err := selector.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "leading or trailing whitespace")
	})
}
