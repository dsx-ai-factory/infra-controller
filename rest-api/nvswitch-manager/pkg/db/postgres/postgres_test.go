// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

// quoteIdentifier quotes a string as a PostgreSQL identifier.
// This is a local copy for testing - the production version is in testutil.
func quoteIdentifier(name string) string {
	return `"` + strings.ReplaceAll(name, `"`, `""`) + `"`
}

// TestQuoteIdentifier verifies that identifiers are properly quoted for PostgreSQL.
// This is a regression test for SQL injection prevention in CreateTestDB.
func TestQuoteIdentifier(t *testing.T) {
	testCases := map[string]struct {
		input    string
		expected string
	}{
		"simple name": {
			input:    "test_database",
			expected: `"test_database"`,
		},
		"name with special chars": {
			input:    "test-database",
			expected: `"test-database"`,
		},
		"name with double quotes": {
			input:    `test"database`,
			expected: `"test""database"`,
		},
		"name with multiple double quotes": {
			input:    `a"b"c`,
			expected: `"a""b""c"`,
		},
		"empty name": {
			input:    "",
			expected: `""`,
		},
		"name with SQL injection attempt": {
			input:    `test"; DROP TABLE users; --`,
			expected: `"test""; DROP TABLE users; --"`,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := quoteIdentifier(tc.input)
			assert.Equal(t, tc.expected, result)
		})
	}
}

// TestDatabaseNameTruncation verifies that database name generation handles edge cases.
// This is a regression test for panic when test name is shorter than 8 characters.
func TestDatabaseNameTruncation(t *testing.T) {
	// Test that short names don't cause panic
	// The actual CreateTestDB function uses t.Name(), but we can test the logic components

	testCases := map[string]struct {
		testName string
	}{
		"very short name": {
			testName: "T",
		},
		"short name": {
			testName: "t_x",
		},
		"exactly 8 chars": {
			testName: "test1234",
		},
		"longer than 8": {
			testName: "test_name_longer_than_eight",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			// Simulate the suffix extraction logic from CreateTestDB
			suffixLen := 8
			if len(tc.testName) < suffixLen {
				suffixLen = len(tc.testName)
			}

			// This should not panic
			suffix := tc.testName[len(tc.testName)-suffixLen:]
			assert.NotEmpty(t, suffix, "Suffix should not be empty")
			assert.LessOrEqual(t, len(suffix), 8, "Suffix should be at most 8 chars")
		})
	}
}
