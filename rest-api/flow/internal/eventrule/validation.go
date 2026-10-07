// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"fmt"
	"regexp"
	"strings"
)

var identifierSegmentRE = regexp.MustCompile(`^[a-z][a-z0-9_]*$`)

// ValidateIdentifier checks that value is a non-empty lower-snake-case
// identifier and uses name to identify the value in validation errors.
func ValidateIdentifier(name string, value string) error {
	if err := validateRequiredString(name, value); err != nil {
		return err
	}
	if !identifierSegmentRE.MatchString(value) {
		return fmt.Errorf(
			"%s %q must start with a lowercase letter and contain only lowercase letters, digits or '_'",
			name,
			value,
		)
	}

	return nil
}

func validateIdentifierPath(name string, value string) error {
	if err := validateRequiredString(name, value); err != nil {
		return err
	}
	for part := range strings.SplitSeq(value, ".") {
		if !identifierSegmentRE.MatchString(part) {
			return fmt.Errorf("%s %q contains invalid path segment %q", name, value, part)
		}
	}

	return nil
}

func validateRequiredString(name string, value string) error {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return fmt.Errorf("%s is empty", name)
	}

	if value != trimmed {
		return fmt.Errorf("%s %q contains leading or trailing whitespace", name, value)
	}

	return nil
}

func validateOptionalString(name string, value string) error {
	if value == "" {
		return nil
	}

	if value != strings.TrimSpace(value) {
		return fmt.Errorf("%s %q contains leading or trailing whitespace", name, value)
	}

	return nil
}

func validateOptionalSlice[T any](name string, values []T) error {
	if values != nil && len(values) == 0 {
		return fmt.Errorf("%s cannot be an empty array", name)
	}

	return nil
}
