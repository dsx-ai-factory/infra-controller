// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestKind_Validate(t *testing.T) {
	t.Run("allows lowercase identifiers", func(t *testing.T) {
		kind := Kind("event.v1_test-kind")

		require.NoError(t, kind.Validate())
	})

	t.Run("rejects numeric leading kind", func(t *testing.T) {
		kind := Kind("1event")

		err := kind.Validate()

		require.Error(t, err)
		assert.ErrorContains(t, err, "must start with a lowercase letter")
	})
}
