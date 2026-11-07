// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestInventoryIntervalFromSchedule(t *testing.T) {
	tests := []struct {
		name     string
		schedule string
		want     time.Duration
		wantErr  bool
	}{
		{
			name:     "every minute descriptor, the co-located Site setting",
			schedule: "@every 1m",
			want:     time.Minute,
		},
		{
			name:     "every three minutes descriptor, the chart default",
			schedule: "@every 3m",
			want:     3 * time.Minute,
		},
		{
			name:     "sub-minute descriptor",
			schedule: "@every 30s",
			want:     30 * time.Second,
		},
		{
			name:     "compound duration",
			schedule: "@every 1h30m",
			want:     90 * time.Minute,
		},
		{
			// The gaps are even here, but accepting it would mean accepting the field syntax
			// whose gaps are not.
			name:     "five field expression is rejected",
			schedule: "*/5 * * * *",
			wantErr:  true,
		},
		{
			// Fires every minute inside hour 9, so the first fire times suggest a 1 minute
			// period while the real gap to the next day is 23 hours.
			name:     "expression whose early gaps hide a long one is rejected",
			schedule: "* 9 * * *",
			wantErr:  true,
		},
		{
			name:     "named descriptor without a period is rejected",
			schedule: "@hourly",
			wantErr:  true,
		},
		{
			name:     "unparseable schedule",
			schedule: "not-a-schedule",
			wantErr:  true,
		},
		{
			name:     "empty schedule",
			schedule: "",
			wantErr:  true,
		},
		{
			name:     "descriptor without a duration",
			schedule: "@every",
			wantErr:  true,
		},
		{
			name:     "descriptor with an unparseable duration",
			schedule: "@every often",
			wantErr:  true,
		},
		{
			// Would otherwise fire continuously and report a zero interval.
			name:     "zero duration is rejected",
			schedule: "@every 0s",
			wantErr:  true,
		},
		{
			name:     "negative duration is rejected",
			schedule: "@every -1m",
			wantErr:  true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := InventoryIntervalFromSchedule(tt.schedule)
			if tt.wantErr {
				require.Error(t, err)
				assert.Zero(t, got)

				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
		})
	}
}
