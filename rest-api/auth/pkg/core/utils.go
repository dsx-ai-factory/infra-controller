// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"strings"

	"github.com/golang-jwt/jwt/v5"
	"github.com/spf13/cast"
)

// =============================================================================
// Constants
// =============================================================================

// ScopeClaims are the standard JWT claim keys used for scopes.
var ScopeClaims = []string{"scope", "scopes", "scp"}

// =============================================================================
// Conversion Functions
// =============================================================================

// InterfaceToStringSlice converts interface{} to []string.
// Supports multiple common formats from various IdPs:
//   - Native array/slice: ["role1", "role2"]
//   - JSON-encoded string array: "[\"role1\", \"role2\"]"
//   - Space-separated: "role1 role2"
//   - Comma-separated: "role1,role2" or "role1, role2"
//   - Semicolon-separated: "role1;role2"
//   - Single value: "role1"
func InterfaceToStringSlice(v any) ([]string, error) {
	if v == nil {
		return nil, nil
	}

	// Handle string values with various formats
	if s, ok := v.(string); ok {
		return parseStringToSlice(s), nil
	}

	// Handle native arrays/slices
	return cast.ToStringSliceE(v)
}

// parseStringToSlice parses a string into a slice using common delimiters.
// Tries formats in order: JSON array, comma-separated, semicolon-separated, space-separated.
func parseStringToSlice(s string) []string {
	trimmed := strings.TrimSpace(s)
	if trimmed == "" {
		return nil
	}

	// Try JSON array format first: ["role1", "role2"]
	if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
		var jsonArray []string
		if err := json.Unmarshal([]byte(trimmed), &jsonArray); err == nil {
			return trimAndFilter(jsonArray)
		}
		// If JSON parsing fails, fall through to other methods
	}

	// Try comma-separated: "role1,role2" or "role1, role2"
	if strings.Contains(trimmed, ",") {
		parts := strings.Split(trimmed, ",")
		return trimAndFilter(parts)
	}

	// Try semicolon-separated: "role1;role2"
	if strings.Contains(trimmed, ";") {
		parts := strings.Split(trimmed, ";")
		return trimAndFilter(parts)
	}

	// Try space/tab/newline-separated: "role1 role2"
	if strings.ContainsAny(trimmed, " \t\n") {
		return strings.Fields(trimmed)
	}

	// Single value
	return []string{trimmed}
}

// trimAndFilter trims whitespace from each element and removes empty strings.
func trimAndFilter(parts []string) []string {
	result := make([]string, 0, len(parts))
	for _, p := range parts {
		trimmed := strings.TrimSpace(p)
		if trimmed != "" {
			result = append(result, trimmed)
		}
	}
	return result
}

// ComputeIssuerPrefix returns SHA256(issuerURL)[0:10] for namespacing subject claims.
func ComputeIssuerPrefix(issuerURL string) string {
	hash := sha256.Sum256([]byte(issuerURL))
	return hex.EncodeToString(hash[:])[:10]
}

// =============================================================================
// Claim Extraction Functions
// =============================================================================

// GetClaimAttribute extracts any value from a nested claim attribute (e.g., "data.roles").
// Returns nil if the attribute is empty or the value is not found.
func GetClaimAttribute(claims jwt.MapClaims, attribute string) any {
	if attribute == "" {
		return nil
	}

	var current any = claims

	for _, key := range strings.Split(attribute, ".") {
		switch m := current.(type) {
		case jwt.MapClaims:
			current = m[key]
		case map[string]any:
			current = m[key]
		default:
			return nil
		}

		if current == nil {
			return nil
		}
	}

	return current
}

// GetClaimAttributeAsString extracts a string from nested claim attributes (e.g., "data.org").
// Accepts multiple attributes and returns the first non-empty string found.
// Returns empty string if none found or if values are not strings.
func GetClaimAttributeAsString(claims jwt.MapClaims, attributes ...string) string {
	for _, attribute := range attributes {
		value := GetClaimAttribute(claims, attribute)
		if str, ok := value.(string); ok && str != "" {
			return str
		}
	}
	return ""
}

// GetScopes extracts scopes from claims (tries "scope", "scopes", "scp").
// Returns a slice of scope strings.
func GetScopes(claims jwt.MapClaims) []string {
	var scopeClaimValue any
	for _, key := range ScopeClaims {
		if val, exists := claims[key]; exists {
			scopeClaimValue = val
			break
		}
	}
	if scopeClaimValue == nil {
		return nil
	}
	scopes, _ := InterfaceToStringSlice(scopeClaimValue)
	return scopes
}
