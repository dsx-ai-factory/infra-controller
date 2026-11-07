// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"hash/fnv"
	"slices"
	"strings"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// GetTypedStrPtr returns a `*string` pointing to a copy of v's
// underlying string value. Accepts any type whose underlying type is
// `string` -- typically a typed-string domain enum like
// `cdbm.MachineCapabilityType` -- so callers can pass typed values to
// DAO filter params that take `*string` without an explicit
// `string(...)` cast at the call site. Distinct from `GetPtr` in that
// it returns `*string` rather than `*T`.
func GetTypedStrPtr[T ~string](v T) *string {
	s := string(v)
	return &s
}

// GetCurTime returns the current time
func GetCurTime() time.Time {
	// Standardize time to match Postgres resolution
	return time.Now().UTC().Round(time.Microsecond)
}

// IsStrInSlice returns true if the provided string is in the provided slice
func IsStrInSlice(s string, sl []string) bool {
	for _, v := range sl {
		if v == s {
			return true
		}
	}
	return false
}

// GetStringToUint64Hash returns a uint64 hash of the input string
// this is used for advisory lock ids
func GetStringToUint64Hash(id string) uint64 {
	h := fnv.New64()
	h.Write([]byte(id))
	return h.Sum64()
}

// GetStringToTsQuery returns a string into a to_tsquery format from the input string
func GetStringToTsQuery(inputQuery string) string {
	inputQuery, ok := TrimSearchQuery(inputQuery)
	if !ok {
		return ""
	}

	tokens := strings.Fields(inputQuery)
	if len(tokens) == 0 {
		return ""
	}

	hasOperator := false
	for _, token := range tokens {
		switch token {
		case "|", "&":
			hasOperator = true
		case "!":
			return ""
		default:
			if strings.ContainsAny(token, "|&!") {
				return ""
			}
		}
	}
	if !hasOperator {
		return strings.Join(tokens, " | ")
	}

	expectTerm := true
	for _, token := range tokens {
		switch token {
		case "|", "&":
			if expectTerm {
				return ""
			}
			expectTerm = true
		default:
			if !expectTerm {
				return ""
			}
			expectTerm = false
		}
	}
	if expectTerm {
		return ""
	}

	return strings.Join(tokens, " ")
}

// normalizeSearchQuery normalizes a search query by trimming it and converting it to a to_tsquery format
func NormalizeSearchQuery(input *string) (string, *string, bool) {
	if input == nil {
		return "", nil, false
	}

	searchQuery, ok := TrimSearchQuery(*input)
	if !ok {
		return "", nil, false
	}

	tsQuery := GetStringToTsQuery(searchQuery)
	if tsQuery == "" {
		return "", nil, false
	}

	return searchQuery, cutil.GetPtr(tsQuery), true
}

// TrimmedSearchQuery trims a search query and reports whether it is non-blank
func TrimSearchQuery(input string) (string, bool) {
	trimmed := strings.TrimSpace(input)
	return trimmed, trimmed != ""
}

// CompareStringSlicesIgnoreOrder compares two string slices ignoring order
func CompareStringSlicesIgnoreOrder(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	// Create sorted copies to compare
	aCopy := make([]string, len(a))
	bCopy := make([]string, len(b))
	copy(aCopy, a)
	copy(bCopy, b)
	slices.Sort(aCopy)
	slices.Sort(bCopy)
	return slices.Equal(aCopy, bCopy)
}
