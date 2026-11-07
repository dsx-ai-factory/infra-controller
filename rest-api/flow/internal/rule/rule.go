// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
)

const (
	maxRuleNameLength      = 128
	maxSchemaVersionLength = 32
)

// Kind identifies a rule family, such as a future event or operation rule kind.
type Kind string

// Validate checks that the kind is safe to persist and use as a registry key.
func (k Kind) Validate() error {
	return validateIdentifier("kind", string(k))
}

// Rule is the shared rule envelope. Definition is interpreted by the
// registered validator for Kind.
type Rule struct {
	ID            uuid.UUID       `json:"id"`
	Kind          Kind            `json:"kind"`
	SchemaVersion string          `json:"schema_version"`
	Definition    json.RawMessage `json:"definition"`
	Name          string          `json:"name"`
	Description   string          `json:"description,omitempty"`
	Selector      Selector        `json:"selector,omitempty"`
	Enabled       bool            `json:"enabled"`
	Priority      int             `json:"priority"`
	CreatedAt     time.Time       `json:"created_at"`
	UpdatedAt     time.Time       `json:"updated_at"`
}

// Validate checks generic rule-envelope constraints, then runs the registered
// kind-specific validator.
func (r *Rule) Validate(registry *Registry) error {
	if err := r.ValidateEnvelope(); err != nil {
		return err
	}

	if registry == nil {
		return fmt.Errorf("rule registry is nil")
	}

	validator, ok := registry.KindValidator(r.Kind)
	if !ok {
		return fmt.Errorf("rule kind %q is not registered", r.Kind)
	}

	if err := validator.Validate(r); err != nil {
		return fmt.Errorf("invalid %s rule: %w", r.Kind, err)
	}

	return nil
}

// ValidateEnvelope checks generic rule-envelope constraints without running
// kind-specific validation.
func (r *Rule) ValidateEnvelope() error {
	if r == nil {
		return fmt.Errorf("rule is nil")
	}

	if err := r.Kind.Validate(); err != nil {
		return err
	}

	if err := validateRequiredString("rule schema_version", r.SchemaVersion, maxSchemaVersionLength); err != nil {
		return err
	}

	if err := validateRequiredJSON("rule definition", r.Definition); err != nil {
		return err
	}

	if err := validateRequiredString("rule name", r.Name, maxRuleNameLength); err != nil {
		return err
	}

	if err := r.Selector.Validate(); err != nil {
		return err
	}

	if r.Priority < 0 {
		return fmt.Errorf("rule priority must be non-negative")
	}

	return nil
}
