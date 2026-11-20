// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package redact hides credentials that decoded JSON bodies carry in URLs.
package redact

import (
	"net/url"
	"strings"
)

// urlParentFields contains the normalized names of fields whose objects, or
// array elements, carry a "url" that can embed credentials, such as
// user:password or a signed query token.
var urlParentFields = map[string]struct{}{
	"artifacts":     {},
	"upgradescript": {},
}

// IsURLParentField reports whether a field's objects, or array elements, carry
// a "url" that can embed credentials. The name is matched without regard to
// case, "-", or "_".
func IsURLParentField(name string) bool {
	normalized := strings.NewReplacer("-", "", "_", "").Replace(strings.ToLower(name))
	_, found := urlParentFields[normalized]
	return found
}

// NestedURLs redacts the "url" of an object, or of each object in an array,
// writing marker in place of each hidden part.
func NestedURLs(value interface{}, marker string) {
	switch typed := value.(type) {
	case map[string]interface{}:
		for key, field := range typed {
			raw, isString := field.(string)
			if isString && strings.EqualFold(key, "url") {
				typed[key] = redactURL(raw, marker)
			}
		}
	case []interface{}:
		for _, element := range typed {
			NestedURLs(element, marker)
		}
	}
}

// redactURL hides a URL's user information, query, and fragment, which can
// carry credentials, and keeps its scheme, host, and path so the artifact stays
// identifiable. A value that does not parse as a URL becomes marker entirely.
func redactURL(raw string, marker string) string {
	parsed, err := url.Parse(raw)
	if err != nil {
		return marker
	}
	if parsed.User != nil {
		// url.User escapes a marker's punctuation, so use a word.
		parsed.User = url.User("redacted")
	}
	if parsed.RawQuery != "" || parsed.ForceQuery {
		parsed.RawQuery = marker
		parsed.ForceQuery = false
	}
	parsed.Fragment, parsed.RawFragment = "", ""
	return parsed.String()
}
