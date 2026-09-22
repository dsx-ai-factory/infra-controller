// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestValidateInventorySchedule(t *testing.T) {
	tests := []struct {
		name     string
		schedule string
		wantErr  string
	}{
		{
			// Empty falls back to the built-in default, which is inside the cap.
			name:     "empty schedule is accepted",
			schedule: "",
		},
		{
			name:     "a schedule faster than the maximum is accepted",
			schedule: "@every 1m",
		},
		{
			name:     "a schedule at the maximum is accepted",
			schedule: "@every 5m",
		},
		{
			name:     "a schedule slower than the maximum is rejected",
			schedule: "@every 6m",
			wantErr:  "slower than the 5m0s maximum",
		},
		{
			// Its gaps alternate 8h and 16h, so no single interval describes it.
			name:     "a field expression is rejected",
			schedule: "0 9,17 * * *",
			wantErr:  `must be an "@every" duration`,
		},
		{
			name:     "an unparseable schedule is rejected",
			schedule: "not-a-schedule",
			wantErr:  `must be an "@every" duration`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validateInventorySchedule(tt.schedule)
			if tt.wantErr == "" {
				require.NoError(t, err)

				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}
