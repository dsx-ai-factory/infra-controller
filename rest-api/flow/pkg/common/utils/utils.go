// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"encoding/json"
	"math/rand"
	"reflect"
	"strings"

	"github.com/rs/zerolog/log"
)

const (
	// Characters to use in the serial number
	charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
)

// GenerateRandomSerial generates a random serial number of specified length
func GenerateRandomSerial(length int) string {
	serial := make([]byte, length)
	for i := range serial {
		serial[i] = charset[rand.Intn(len(charset))]
	}

	return string(serial)
}

// StructToMap converts a struct to a map[string]any using reflection. It
// handles nested structs recursively and respects json tags and returns
// empty map for non-struct inputs
func StructToMap(obj any) map[string]any {
	m := make(map[string]any)

	v := reflect.ValueOf(obj)
	if v.Kind() == reflect.Pointer {
		v = v.Elem()
	}
	if v.Kind() != reflect.Struct {
		log.Debug().Msgf("StructToMap: input is not struct, got %v", v.Kind())
		return m
	}

	t := reflect.TypeOf(obj)
	for i := range v.NumField() {
		field := t.Field(i)
		fieldValue := v.Field(i)

		// Skip unexported fields
		if !fieldValue.CanInterface() {
			continue
		}

		// Use json tag if available, otherwise use field name
		key := field.Name
		if tag := field.Tag.Get("json"); tag != "" && tag != "-" {
			// Handle json tag with options (e.g., "name,omitempty")
			if commaIdx := strings.Index(tag, ","); commaIdx != -1 {
				key = tag[:commaIdx]
			} else {
				key = tag
			}
		}

		// Handle nested struct or pointer to struct recursively
		fieldVal := fieldValue
		if fieldVal.Kind() == reflect.Pointer {
			if fieldVal.IsNil() {
				m[key] = nil
				continue
			}
			fieldVal = fieldVal.Elem()
		}

		if fieldVal.Kind() == reflect.Struct {
			m[key] = StructToMap(fieldVal.Interface())
		} else {
			m[key] = fieldValue.Interface()
		}
	}

	return m
}

// JSONStringToMap tries to parse a JSON string `s` into a map.
// If parsing fails, it returns a map with the original string as a value
// uder key `n`
func JSONStringToMap(n string, s string) map[string]any {
	if s == "" {
		return nil
	}

	var m map[string]any

	if err := json.Unmarshal([]byte(s), &m); err != nil {
		log.Debug().Err(err).Str("input", s).Msg("Failed to parse JSON string")
		m = map[string]any{n: s}
	}

	return m
}

// StringToMap tries to parse a string `s` into a map.
// If parsing fails, it returns nil.
func StringToMap(s string) map[string]any {
	if s == "" {
		return nil
	}

	var m map[string]any

	if err := json.Unmarshal([]byte(s), &m); err != nil {
		log.Debug().Err(err).Str("input", s).Msg("Failed to parse JSON string")
		return nil
	}

	return m
}

// MapToJSONString converts a map to its JSON string representation.
// Returns empty string if marshaling fails.
func MapToJSONString(m map[string]any) string {
	if m == nil {
		return ""
	}

	b, err := json.Marshal(m)
	if err != nil {
		log.Debug().Msgf("Error marshaling map to JSON: %v", err)
		return ""
	}

	return string(b)
}

// CompareAndCopyMaps compares two map[string]any for deep equality.
// If they are different, it returns a deep copy of the first map.
// If they are the same, it returns nil.
func CompareAndCopyMaps(src, tgt map[string]any) map[string]any {
	if src == nil {
		return nil
	}

	if tgt != nil && reflect.DeepEqual(src, tgt) {
		return nil
	}

	return DeepCopyMap(src)
}

// DeepCopyMap creates a deep copy of a map[string]any, handling nested
// maps, slices, and other complex types recursively.
func DeepCopyMap(original map[string]any) map[string]any {
	if original == nil {
		return nil
	}

	copy := make(map[string]any, len(original))
	for key, value := range original {
		copy[key] = deepCopyValue(value)
	}
	return copy
}

// deepCopyValue recursively copies a value of any type
func deepCopyValue(value any) any {
	if value == nil {
		return nil
	}

	v := reflect.ValueOf(value)
	switch v.Kind() {
	case reflect.Map:
		if v.Type().Key().Kind() == reflect.String && v.Type().Elem().Kind() == reflect.Interface {
			// Handle map[string]any specifically
			original := value.(map[string]any)
			return DeepCopyMap(original)
		}
		// Handle other map types
		mapCopy := reflect.MakeMap(v.Type())
		for _, key := range v.MapKeys() {
			mapCopy.SetMapIndex(key, reflect.ValueOf(deepCopyValue(v.MapIndex(key).Interface())))
		}
		return mapCopy.Interface()
	case reflect.Slice:
		sliceCopy := reflect.MakeSlice(v.Type(), v.Len(), v.Cap())
		for i := range v.Len() {
			sliceCopy.Index(i).Set(reflect.ValueOf(deepCopyValue(v.Index(i).Interface())))
		}
		return sliceCopy.Interface()
	case reflect.Pointer:
		if v.IsNil() {
			return nil
		}
		ptrCopy := reflect.New(v.Elem().Type())
		ptrCopy.Elem().Set(reflect.ValueOf(deepCopyValue(v.Elem().Interface())))
		return ptrCopy.Interface()
	case reflect.Struct:
		structCopy := reflect.New(v.Type()).Elem()
		for i := range v.NumField() {
			if structCopy.Field(i).CanSet() {
				structCopy.Field(i).Set(reflect.ValueOf(deepCopyValue(v.Field(i).Interface())))
			}
		}
		return structCopy.Interface()
	default:
		// For basic types (string, int, bool, etc.), return as-is since they're copied by value
		return value
	}
}
