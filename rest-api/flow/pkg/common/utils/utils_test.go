// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestStructToMap(t *testing.T) {
	type Nested struct {
		Field1 string
		Field2 int
	}

	type TestStruct struct {
		SimpleField string
		NestedField Nested
	}

	test := TestStruct{
		SimpleField: "value",
		NestedField: Nested{
			Field1: "nested_value",
			Field2: 42,
		},
	}

	m := StructToMap(test)
	assert.Equal(t, map[string]any{
		"SimpleField": "value",
		"NestedField": map[string]any{
			"Field1": "nested_value",
			"Field2": 42,
		},
	}, m)
}

func TestJSONStringToMap(t *testing.T) {
	testCases := map[string]struct {
		input    string
		expected map[string]any
	}{
		"valid input": {
			input: `{"key1":"value1","key2":"2"}`,
			expected: map[string]any{
				"key1": "value1",
				"key2": "2",
			},
		},
		"invalid input": {
			input: "invalid input",
			expected: map[string]any{
				"json": "invalid input",
			},
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			m := JSONStringToMap("json", testCase.input)
			assert.Equal(t, testCase.expected, m)
		})
	}
}

func TestMapToJSONString(t *testing.T) {
	m := map[string]any{
		"key1": "value1",
		"key2": 2,
	}

	jsonStr := MapToJSONString(m)
	assert.JSONEq(t, `{"key1":"value1","key2":2}`, jsonStr)

	invalidMap := map[string]any{
		"key": func() {}, // functions cannot be marshaled to JSON
	}

	jsonStr = MapToJSONString(invalidMap)
	assert.Equal(t, "", jsonStr)
}

func TestCompareAndCopyMaps(t *testing.T) {
	tests := []struct {
		name     string
		map1     map[string]any
		map2     map[string]any
		expected map[string]any // nil if maps should be equal or map1 is nil
	}{
		{
			name:     "identical simple maps",
			map1:     map[string]any{"a": 1, "b": "hello"},
			map2:     map[string]any{"a": 1, "b": "hello"},
			expected: nil,
		},
		{
			name:     "different simple maps",
			map1:     map[string]any{"a": 1, "b": "hello"},
			map2:     map[string]any{"a": 2, "b": "hello"},
			expected: map[string]any{"a": 1, "b": "hello"}, // Returns copy of map1
		},
		{
			name:     "both nil maps",
			map1:     nil,
			map2:     nil,
			expected: nil,
		},
		{
			name:     "map1 nil, map2 not nil",
			map1:     nil,
			map2:     map[string]any{"a": 1},
			expected: nil, // Always returns nil when map1 is nil
		},
		{
			name:     "map1 not nil, map2 nil",
			map1:     map[string]any{"a": 1},
			map2:     nil,
			expected: map[string]any{"a": 1}, // Returns copy of map1
		},
		{
			name:     "empty maps",
			map1:     map[string]any{},
			map2:     map[string]any{},
			expected: nil,
		},
		{
			name: "nested maps identical",
			map1: map[string]any{
				"outer": map[string]any{
					"inner": "value",
					"num":   42,
				},
			},
			map2: map[string]any{
				"outer": map[string]any{
					"inner": "value",
					"num":   42,
				},
			},
			expected: nil,
		},
		{
			name: "nested maps different",
			map1: map[string]any{
				"outer": map[string]any{
					"inner": "value1",
					"num":   42,
				},
			},
			map2: map[string]any{
				"outer": map[string]any{
					"inner": "value2",
					"num":   42,
				},
			},
			expected: map[string]any{
				"outer": map[string]any{
					"inner": "value1", // Returns copy of map1, not map2
					"num":   42,
				},
			},
		},
		{
			name:     "maps with slices",
			map1:     map[string]any{"slice": []int{1, 2, 3}},
			map2:     map[string]any{"slice": []int{1, 2, 4}},
			expected: map[string]any{"slice": []int{1, 2, 3}}, // Returns copy of map1
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := CompareAndCopyMaps(tt.map1, tt.map2)

			if tt.expected == nil {
				assert.Nil(t, result, "Expected nil for identical maps or nil map1")
			} else {
				assert.NotNil(t, result, "Expected non-nil for different maps")
				assert.Equal(t, tt.expected, result, "Returned map should match expected")

				// Verify it's a copy by modifying original map1 and checking copy is unchanged
				if tt.map1 != nil {
					if nested, ok := tt.map1["outer"].(map[string]any); ok {
						nested["modified"] = "test"
						// The result should not be affected
						if resultNested, ok := result["outer"].(map[string]any); ok {
							assert.NotContains(t, resultNested, "modified", "Copy should be independent")
						}
					}
				}
			}
		})
	}
}

func TestDeepCopyMap(t *testing.T) {
	tests := []struct {
		name     string
		input    map[string]any
		expected map[string]any
	}{
		{
			name:     "nil map",
			input:    nil,
			expected: nil,
		},
		{
			name:     "empty map",
			input:    map[string]any{},
			expected: map[string]any{},
		},
		{
			name: "simple map",
			input: map[string]any{
				"string": "hello",
				"int":    42,
				"bool":   true,
				"nil":    nil,
			},
			expected: map[string]any{
				"string": "hello",
				"int":    42,
				"bool":   true,
				"nil":    nil,
			},
		},
		{
			name: "nested maps",
			input: map[string]any{
				"level1": map[string]any{
					"level2": map[string]any{
						"value": "deep",
					},
					"simple": "value",
				},
			},
			expected: map[string]any{
				"level1": map[string]any{
					"level2": map[string]any{
						"value": "deep",
					},
					"simple": "value",
				},
			},
		},
		{
			name: "map with slices",
			input: map[string]any{
				"numbers": []int{1, 2, 3},
				"strings": []string{"a", "b", "c"},
				"mixed":   []any{1, "two", true},
			},
			expected: map[string]any{
				"numbers": []int{1, 2, 3},
				"strings": []string{"a", "b", "c"},
				"mixed":   []any{1, "two", true},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := DeepCopyMap(tt.input)
			assert.Equal(t, tt.expected, result, "Copy should match original")

			// Test that it's a deep copy by modifying nested structures
			if tt.input != nil && result != nil {
				// Test nested map independence
				if nested, ok := tt.input["level1"].(map[string]any); ok {
					nested["new_key"] = "modified"
					resultNested := result["level1"].(map[string]any)
					assert.NotContains(t, resultNested, "new_key", "Nested map should be independent")
				}

				// Test slice independence
				if slice, ok := tt.input["numbers"].([]int); ok {
					if len(slice) > 0 {
						slice[0] = 999
						resultSlice := result["numbers"].([]int)
						assert.NotEqual(t, 999, resultSlice[0], "Slice should be independent")
					}
				}
			}
		})
	}
}

func TestDeepCopyMapWithComplexStructures(t *testing.T) {
	// Test with pointers and structs
	type TestStruct struct {
		Value string
		Num   int
	}

	ptrValue := &TestStruct{Value: "pointer", Num: 123}
	structValue := TestStruct{Value: "struct", Num: 456}

	input := map[string]any{
		"pointer": ptrValue,
		"struct":  structValue,
		"nested": map[string]any{
			"slice_of_maps": []map[string]any{
				{"key1": "value1"},
				{"key2": "value2"},
			},
		},
	}

	result := DeepCopyMap(input)

	// Verify the copy
	assert.Equal(t, input, result, "Complex copy should match original")

	// Test independence by modifying original
	ptrValue.Value = "modified"
	resultPtr := result["pointer"].(*TestStruct)
	assert.Equal(t, "pointer", resultPtr.Value, "Pointer copy should be independent")

	// Test nested slice of maps
	nestedOriginal := input["nested"].(map[string]any)["slice_of_maps"].([]map[string]any)
	nestedOriginal[0]["modified"] = true

	nestedResult := result["nested"].(map[string]any)["slice_of_maps"].([]map[string]any)
	assert.NotContains(t, nestedResult[0], "modified", "Nested slice of maps should be independent")
}
