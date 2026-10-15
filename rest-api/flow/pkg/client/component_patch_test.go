// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestComponentPositionPatch(t *testing.T) {
	zero := int32(0)
	for _, tc := range []struct {
		name      string
		opts      PatchComponentOpts
		wantNil   bool
		wantPaths []string
	}{
		{
			name:      "explicit zero preserves omitted coordinates",
			opts:      PatchComponentOpts{SlotID: &zero},
			wantPaths: []string{"position.slot_id"},
		},
		{
			name:    "omitted",
			wantNil: true,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			position, mask := componentPositionPatch(tc.opts)
			if tc.wantNil {
				assert.Nil(t, position)
				assert.Nil(t, mask)
				return
			}

			require.NotNil(t, position)
			require.NotNil(t, mask)
			assert.Equal(t, int32(0), position.SlotId)
			assert.Equal(t, tc.wantPaths, mask.Paths)
		})
	}
}
