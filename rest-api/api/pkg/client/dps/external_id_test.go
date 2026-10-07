// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestExternalIDFromVPCID(t *testing.T) {
	tests := []struct {
		name      string
		vpcID     string
		want      int64
		wantError string
	}{
		{
			name:  "derives ID from first eight UUID bytes",
			vpcID: "00000000-0000-0002-8000-000000000000",
			want:  2,
		},
		{
			name:  "normalizes zero to one",
			vpcID: "80000000-0000-0000-8000-000000000000",
			want:  1,
		},
		{
			name:  "trims UUID whitespace",
			vpcID: " 00000000-0000-0002-8000-000000000000 ",
			want:  2,
		},
		{
			name:      "rejects invalid UUID",
			vpcID:     "not-a-uuid",
			wantError: "parse VPC ID",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := ExternalIDFromVPCID(test.vpcID)
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				return
			}

			require.NoError(t, err)
			assert.Equal(t, test.want, got)
			assert.Positive(t, got)
		})
	}
}
