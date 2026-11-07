// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package location

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestLocation(t *testing.T) {
	loc := Location{
		Region:     "NA",
		DataCenter: "DC1",
		Room:       "Room1",
		Position:   "Pos1",
	}

	jsonData, err := json.Marshal(loc)
	assert.NoError(t, err)

	newLoc := New(jsonData)
	assert.Equal(t, loc, newLoc)

	invalidJSON := []byte("invalid")
	unknownLoc := New(invalidJSON)
	assert.Equal(t, "", unknownLoc.Region)
	assert.Equal(t, "", unknownLoc.DataCenter)
	assert.Equal(t, "", unknownLoc.Room)
	assert.Equal(t, "", unknownLoc.Position)
}

func TestLocation_ToMap(t *testing.T) {
	tests := map[string]struct {
		loc      *Location
		expected map[string]any
	}{
		"nil location": {
			loc:      nil,
			expected: nil,
		},
		"all fields empty": {
			loc:      &Location{},
			expected: nil,
		},
		"all fields populated": {
			loc: &Location{
				Region:     "NA",
				DataCenter: "DC1",
				Room:       "Room1",
				Position:   "Rack-A1",
			},
			expected: map[string]any{
				"region":      "NA",
				"data_center": "DC1",
				"room":        "Room1",
				"position":    "Rack-A1",
			},
		},
		"only region": {
			loc: &Location{Region: "EU"},
			expected: map[string]any{
				"region": "EU",
			},
		},
		"region and room": {
			loc: &Location{Region: "NA", Room: "Room1"},
			expected: map[string]any{
				"region": "NA",
				"room":   "Room1",
			},
		},
		"only position": {
			loc: &Location{Position: "Rack-B2"},
			expected: map[string]any{
				"position": "Rack-B2",
			},
		},
		"three of four fields": {
			loc: &Location{
				Region:     "APAC",
				DataCenter: "DC3",
				Room:       "Server-Room-7",
			},
			expected: map[string]any{
				"region":      "APAC",
				"data_center": "DC3",
				"room":        "Server-Room-7",
			},
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			result := tc.loc.ToMap()
			if tc.expected == nil {
				assert.Nil(t, result)
			} else {
				require.NotNil(t, result)
				assert.Equal(t, tc.expected, result)
			}
		})
	}
}
