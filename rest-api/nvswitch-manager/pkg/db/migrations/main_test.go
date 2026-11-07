// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import "testing"

// TestParseMigrationFilename_MalformedFilenames verifies that parseMigrationFilename
// correctly rejects malformed migration filenames.
// This is a regression test for: parseMigrationFilename returning ok=true for invalid filenames.
func TestParseMigrationFilename_MalformedFilenames(t *testing.T) {
	testCases := map[string]struct {
		path     string
		isUp     bool
		expectOk bool
		expectID string
		expectNm string
	}{
		// Valid cases
		"valid up migration": {
			path:     "202509290356_initial.up.sql",
			isUp:     true,
			expectOk: true,
			expectID: "202509290356",
			expectNm: "initial",
		},
		"valid down migration": {
			path:     "202509290356_initial.down.sql",
			isUp:     false,
			expectOk: true,
			expectID: "202509290356",
			expectNm: "initial",
		},

		// Invalid suffix cases
		"up migration with wrong suffix": {
			path:     "202509290356_initial.down.sql",
			isUp:     true, // looking for .up.sql but file is .down.sql
			expectOk: false,
		},
		"down migration with wrong suffix": {
			path:     "202509290356_initial.up.sql",
			isUp:     false, // looking for .down.sql but file is .up.sql
			expectOk: false,
		},
		"missing sql extension": {
			path:     "202509290356_initial.up",
			isUp:     true,
			expectOk: false,
		},
		"wrong extension": {
			path:     "202509290356_initial.up.txt",
			isUp:     true,
			expectOk: false,
		},

		// Malformed filename cases (regression tests)
		"extra underscore in name - should reject": {
			path:     "202509290356_initial_extra.up.sql",
			isUp:     true,
			expectOk: false, // Previously returned ok=true with "Invalid filename"
		},
		"multiple underscores - should reject": {
			path:     "202509290356_add_users_table.up.sql",
			isUp:     true,
			expectOk: false, // Should be rejected due to multiple underscores
		},
		"no underscore - should reject": {
			path:     "202509290356initial.up.sql",
			isUp:     true,
			expectOk: false,
		},
		"only ID no name - should reject": {
			path:     "202509290356.up.sql",
			isUp:     true,
			expectOk: false,
		},
		"empty filename": {
			path:     ".up.sql",
			isUp:     true,
			expectOk: false,
		},

		// Directory and non-file cases
		"directory path": {
			path:     ".",
			isUp:     true,
			expectOk: false,
		},
		"random file": {
			path:     "README.md",
			isUp:     true,
			expectOk: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			id, nm, ok := parseMigrationFilename(tc.path, tc.isUp)

			if ok != tc.expectOk {
				t.Errorf("parseMigrationFilename(%q, %v) ok = %v, want %v (id=%q, name=%q)",
					tc.path, tc.isUp, ok, tc.expectOk, id, nm)
				return
			}

			if tc.expectOk {
				if id != tc.expectID {
					t.Errorf("parseMigrationFilename(%q, %v) id = %q, want %q",
						tc.path, tc.isUp, id, tc.expectID)
				}
				if nm != tc.expectNm {
					t.Errorf("parseMigrationFilename(%q, %v) name = %q, want %q",
						tc.path, tc.isUp, nm, tc.expectNm)
				}
			}
		})
	}
}

// TestAlternatePresent verifies the alternatePresent function works correctly.
func TestAlternatePresent(t *testing.T) {
	testCases := map[string]struct {
		path     string
		expected bool
	}{
		"up migration has matching down": {
			path:     "202501290000_nvswitch.up.sql",
			expected: true,
		},
		"down migration has matching up": {
			path:     "202501290000_nvswitch.down.sql",
			expected: true,
		},
		"non-existent migration": {
			path:     "999999999999_nonexistent.up.sql",
			expected: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := alternatePresent(tc.path)
			if result != tc.expected {
				t.Errorf("alternatePresent(%q) = %v, want %v", tc.path, result, tc.expected)
			}
		})
	}
}

// TestStringHash verifies the hash function produces consistent results.
func TestStringHash(t *testing.T) {
	testCases := map[string]struct {
		input    []byte
		expected string
	}{
		"empty input": {
			input:    []byte(""),
			expected: "d41d8cd98f00b204e9800998ecf8427e", // MD5 of empty string
		},
		"simple content": {
			input:    []byte("CREATE TABLE test;"),
			expected: stringHash([]byte("CREATE TABLE test;")), // Self-consistent check
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := stringHash(tc.input)
			if result != tc.expected {
				t.Errorf("stringHash(%q) = %q, want %q", string(tc.input), result, tc.expected)
			}
		})
	}

	// Verify consistency - same input always produces same output
	t.Run("consistency check", func(t *testing.T) {
		input := []byte("SELECT * FROM users;")
		hash1 := stringHash(input)
		hash2 := stringHash(input)
		if hash1 != hash2 {
			t.Errorf("stringHash is not consistent: %q != %q", hash1, hash2)
		}
	})
}

// TestHashMatch verifies hash comparison works correctly.
func TestHashMatch(t *testing.T) {
	content := []byte("CREATE TABLE users (id SERIAL PRIMARY KEY);")
	correctHash := stringHash(content)
	wrongHash := "incorrect_hash_value"

	if !hashMatch(content, correctHash) {
		t.Error("hashMatch should return true for matching hash")
	}

	if hashMatch(content, wrongHash) {
		t.Error("hashMatch should return false for non-matching hash")
	}
}
