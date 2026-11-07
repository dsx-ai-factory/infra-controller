// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"bytes"
	"encoding/json"
	"fmt"
	"regexp"
	"strings"
)

var identifierRE = regexp.MustCompile(`^[a-z][a-z0-9_.-]*$`)

func validateIdentifier(name string, value string) error {
	if err := validateTrimmedString(name, value); err != nil {
		return err
	}

	if !identifierRE.MatchString(value) {
		return fmt.Errorf(
			"%s %q must start with a lowercase letter and contain only lowercase letters, digits, '.', '_' or '-'",
			name,
			value,
		)
	}

	return nil
}

func validateRequiredJSON(name string, value json.RawMessage) error {
	trimmed := bytes.TrimSpace(value)
	if len(trimmed) == 0 {
		return fmt.Errorf("%s is required", name)
	}

	if bytes.Equal(trimmed, []byte("null")) {
		return fmt.Errorf("%s cannot be null", name)
	}

	if !json.Valid(trimmed) {
		return fmt.Errorf("%s must be valid JSON", name)
	}

	return nil
}

func validateRequiredString(name string, value string, maxLength int) error {
	if err := validateTrimmedString(name, value); err != nil {
		return err
	}

	if len(value) > maxLength {
		return fmt.Errorf("%s exceeds %d characters", name, maxLength)
	}

	return nil
}

func validateTrimmedString(name string, value string) error {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return fmt.Errorf("%s is empty", name)
	}

	if value != trimmed {
		return fmt.Errorf("%s %q contains leading or trailing whitespace", name, value)
	}

	return nil
}
