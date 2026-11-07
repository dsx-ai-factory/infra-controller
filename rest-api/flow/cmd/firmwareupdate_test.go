// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
)

func TestParseTimeString(t *testing.T) {
	// Fixed reference time for consistent testing
	referenceYear := 2025
	referenceMonth := time.January
	referenceDay := 2
	referenceHour := 3
	referenceMinute := 4
	referenceSecond := 5

	testCases := map[string]struct {
		input       string
		expectError bool
		validateFn  func(t *testing.T, result time.Time)
	}{
		"with timezone offset +0000 (UTC)": {
			input:       "2025-01-02T03:04:05+0000",
			expectError: false,
			validateFn: func(t *testing.T, result time.Time) {
				assert.Equal(t, referenceYear, result.Year())
				assert.Equal(t, referenceMonth, result.Month())
				assert.Equal(t, referenceDay, result.Day())
				assert.Equal(t, referenceHour, result.Hour())
				assert.Equal(t, referenceMinute, result.Minute())
				assert.Equal(t, referenceSecond, result.Second())
				_, offset := result.Zone()
				assert.Equal(t, 0, offset) // UTC offset is 0
			},
		},
		"with positive timezone offset +0800": {
			input:       "2025-06-15T14:30:00+0800",
			expectError: false,
			validateFn: func(t *testing.T, result time.Time) {
				assert.Equal(t, 2025, result.Year())
				assert.Equal(t, time.June, result.Month())
				assert.Equal(t, 15, result.Day())
				assert.Equal(t, 14, result.Hour())
				_, offset := result.Zone()
				assert.Equal(t, 8*3600, offset) // +8 hours in seconds
			},
		},
		"with negative timezone offset -0500": {
			input:       "2025-12-31T23:59:59-0500",
			expectError: false,
			validateFn: func(t *testing.T, result time.Time) {
				assert.Equal(t, 2025, result.Year())
				assert.Equal(t, time.December, result.Month())
				assert.Equal(t, 31, result.Day())
				assert.Equal(t, 23, result.Hour())
				assert.Equal(t, 59, result.Minute())
				assert.Equal(t, 59, result.Second())
				_, offset := result.Zone()
				assert.Equal(t, -5*3600, offset) // -5 hours in seconds
			},
		},
		"without timezone (local time)": {
			input:       "2025-01-02T03:04:05",
			expectError: false,
			validateFn: func(t *testing.T, result time.Time) {
				assert.Equal(t, referenceYear, result.Year())
				assert.Equal(t, referenceMonth, result.Month())
				assert.Equal(t, referenceDay, result.Day())
				assert.Equal(t, referenceHour, result.Hour())
				assert.Equal(t, referenceMinute, result.Minute())
				assert.Equal(t, referenceSecond, result.Second())
				// Should be in local timezone
				assert.Equal(t, time.Local, result.Location())
			},
		},
		"invalid: date only without time": {
			input:       "2025-01-02",
			expectError: true,
		},
		"invalid: space separator instead of T": {
			input:       "2025-01-02 03:04:05",
			expectError: true,
		},
		"invalid: random string": {
			input:       "not a time",
			expectError: true,
		},
		"invalid: empty string": {
			input:       "",
			expectError: true,
		},
		"invalid: timezone with colon (RFC3339)": {
			input:       "2025-01-02T03:04:05+00:00",
			expectError: true,
		},
		"invalid: timezone with Z suffix": {
			input:       "2025-01-02T03:04:05Z",
			expectError: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result, err := parseTimeString(tc.input)

			if tc.expectError {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), "unable to parse time")
			} else {
				assert.NoError(t, err)
				if tc.validateFn != nil {
					tc.validateFn(t, result)
				}
			}
		})
	}
}

func TestParseTimeStringTimezoneConversion(t *testing.T) {
	// Verify that the same moment in different timezones produces correct UTC equivalence
	testCases := map[string]struct {
		time1 string
		time2 string
	}{
		"UTC vs +0800": {
			time1: "2025-01-02T12:00:00+0000", // 12:00 UTC
			time2: "2025-01-02T20:00:00+0800", // 20:00 +0800 = 12:00 UTC
		},
		"UTC vs -0500": {
			time1: "2025-01-02T12:00:00+0000", // 12:00 UTC
			time2: "2025-01-02T07:00:00-0500", // 07:00 -0500 = 12:00 UTC
		},
		"+0800 vs -0500": {
			time1: "2025-01-02T20:00:00+0800", // 12:00 UTC
			time2: "2025-01-02T07:00:00-0500", // 12:00 UTC
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			t1, err1 := parseTimeString(tc.time1)
			t2, err2 := parseTimeString(tc.time2)

			assert.NoError(t, err1)
			assert.NoError(t, err2)
			assert.Equal(t, t1.UTC(), t2.UTC(), "Times should represent the same moment in UTC")
		})
	}
}
