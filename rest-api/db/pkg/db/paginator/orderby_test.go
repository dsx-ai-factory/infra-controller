// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package paginator

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestOrderBy_FromAPIRequest(t *testing.T) {
	cases := []struct {
		input   string
		want    OrderBy
		invalid bool
	}{
		{input: "TYPE_ASC", want: OrderBy{Field: "type", Order: OrderAscending}},
		{input: "DISPLAY_NAME_DESC", want: OrderBy{Field: "display_name", Order: OrderDescending}},
		{input: "", invalid: true},
		{input: "TYPE", invalid: true},
		{input: "_ASC", invalid: true},
		{input: "type_ASC", invalid: true},
		{input: "TYPE_INVALID", invalid: true},
	}
	for _, tc := range cases {
		t.Run(tc.input, func(t *testing.T) {
			original := OrderBy{Field: "name", Order: OrderDescending}
			got := original
			err := got.FromAPIRequest(tc.input)
			if tc.invalid {
				require.ErrorIs(t, err, ErrInvalidOrderBy)
				assert.Equal(t, original, got)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

func TestOrderBy_ToAPIRequest(t *testing.T) {
	cases := []struct {
		order OrderBy
		want  string
	}{
		{order: *NewDefaultOrderBy("type"), want: "TYPE_ASC"},
		{order: OrderBy{Field: "display_name", Order: OrderDescending}, want: "DISPLAY_NAME_DESC"},
	}
	for _, tc := range cases {
		t.Run(tc.want, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.order.ToAPIRequest())
		})
	}
}
