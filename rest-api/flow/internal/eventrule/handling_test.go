// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestHandlingCursor(t *testing.T) {
	want := HandlingCursor{
		HandledAt: time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC),
		ID:        uuid.New(),
	}
	t.Run("round trip", func(t *testing.T) {
		encoded, err := EncodeHandlingCursor(want)
		require.NoError(t, err)
		got, err := DecodeHandlingCursor(encoded)
		require.NoError(t, err)
		require.Equal(t, want, *got)
	})
	t.Run("empty", func(t *testing.T) {
		got, err := DecodeHandlingCursor("")
		require.NoError(t, err)
		require.Nil(t, got)
	})
	t.Run("invalid", func(t *testing.T) {
		_, err := DecodeHandlingCursor("not-a-cursor")
		require.Error(t, err)
	})
}

func TestHandlingCursor_Validate(t *testing.T) {
	valid := HandlingCursor{HandledAt: time.Now(), ID: uuid.New()}
	tests := map[string]struct {
		cursor  HandlingCursor
		wantErr string
	}{
		"valid":                {cursor: valid},
		"missing handled time": {cursor: HandlingCursor{ID: valid.ID}, wantErr: "handling cursor handled time is required"},
		"missing ID":           {cursor: HandlingCursor{HandledAt: valid.HandledAt}, wantErr: "handling cursor ID is required"},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.cursor.Validate()
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}
