// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestComponentPositionFromFlags(t *testing.T) {
	for _, tc := range []struct {
		name string
		set  map[string]string
		want types.InRackPosition
	}{
		{
			name: "omitted position remains unknown",
			want: types.InRackPosition{
				SlotID:    unknownComponentPosition,
				TrayIndex: unknownComponentPosition,
				HostID:    unknownComponentPosition,
			},
		},
		{
			name: "explicit zeros remain valid",
			set: map[string]string{
				"slot-id":    "0",
				"tray-index": "0",
				"host-id":    "0",
			},
			want: types.InRackPosition{},
		},
		{
			name: "partial position preserves omitted coordinates as unknown",
			set:  map[string]string{"slot-id": "7"},
			want: types.InRackPosition{
				SlotID:    7,
				TrayIndex: unknownComponentPosition,
				HostID:    unknownComponentPosition,
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cmd := newAddCmd()
			for name, value := range tc.set {
				require.NoError(t, cmd.Flags().Set(name, value))
			}
			assert.Equal(t, tc.want, componentPositionFromFlags(cmd))
		})
	}
}
