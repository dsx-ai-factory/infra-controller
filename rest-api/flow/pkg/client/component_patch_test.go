// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestComponentPositionPatch_ExplicitZeroPreservesOmittedCoordinates(t *testing.T) {
	zero := int32(0)
	position, mask := componentPositionPatch(PatchComponentOpts{SlotID: &zero})

	require.NotNil(t, position)
	require.NotNil(t, mask)
	assert.Equal(t, int32(0), position.SlotId)
	assert.Equal(t, []string{"position.slot_id"}, mask.Paths)
}

func TestComponentPositionPatch_Omitted(t *testing.T) {
	position, mask := componentPositionPatch(PatchComponentOpts{})

	assert.Nil(t, position)
	assert.Nil(t, mask)
}
